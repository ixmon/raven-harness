//! Basic ratatui chat TUI for the agent.
//!
//! - Scrollable message history (user, assistant, tool results, errors)
//! - Bottom input bar
//! - Live streaming of assistant tokens when possible
//! - Tool execution feedback shown inline

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    Terminal,
};
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::mpsc;

use crate::agent::Agent;
use crate::config::Config;
use crate::input_dispatch::{
    apply_settings_actions, default_slash_commands, dispatch_slash_command, SlashContext,
    SlashDispatch,
};
use crate::llm::{StreamChunk, ToolCall};
use crate::search::SearchState;
use crate::settings_modal::{handle_settings_key, SettingsModal};

#[cfg(feature = "clipboard")]
use arboard::Clipboard;

fn get_filtered_commands<'a>(
    commands: &'a [crate::input_dispatch::SlashCommand],
    input: &str,
) -> Vec<&'a crate::input_dispatch::SlashCommand> {
    if !input.starts_with('/') {
        return vec![];
    }
    let prefix = &input[1..].to_lowercase();
    commands
        .iter()
        .filter(|cmd| prefix.is_empty() || cmd.name.starts_with(prefix))
        .collect()
}

fn clamp_slash_selection(
    commands: &[crate::input_dispatch::SlashCommand],
    input: &str,
    selected: &mut usize,
) {
    let filtered = get_filtered_commands(commands, input);
    if !filtered.is_empty() {
        *selected = (*selected).min(filtered.len().saturating_sub(1));
    } else {
        *selected = 0;
    }
}

#[cfg(feature = "clipboard")]
fn try_copy_to_clipboard(text: &str) {
    if let Ok(mut clipboard) = Clipboard::new() {
        let _ = clipboard.set_text(text.to_string());
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Pane {
    Left,
    Right,
}

/// Holds all the mutable UI state that used to be ~50 local `let mut` variables
/// inside `run_app`. This is the main refactor from the glm.md review.
struct App {
    // Conversation / output
    left_committed: Vec<String>,
    current_response: String,

    // Right pane
    trace_lines: Vec<String>,
    current_thinking: String,

    // Input
    input: String,

    // Navigation / scroll
    left_scroll: u16,
    right_scroll: u16,
    left_follow_output: bool,
    right_follow_output: bool,
    focused_pane: Pane,
    scroll_flash_timer: u8,

    // Processing state
    is_processing: bool,
    spinner_tick: usize,
    tool_calls_this_turn: usize,
    turn_rounds: usize,
    ctx_used_tokens: u32,

    // Approval
    pending_approval: Option<String>,
    approval_responder: Option<tokio::sync::oneshot::Sender<bool>>,
    needs_redraw: bool,

    // Mode menu
    mode_menu_active: bool,
    selected_mode_idx: usize,
    approval_modes: [&'static str; 4],

    // Slash menu
    slash_commands: Vec<crate::input_dispatch::SlashCommand>,
    slash_selected: usize,

    // Display state (updated on endpoint switch)
    display_model: String,
    display_budget: crate::config::ContextBudget,

    // Settings modal (extracted module)
    settings: SettingsModal,

    // Last draw layout (for scroll clamping in keys)
    last_left_line_count: u16,
    last_right_line_count: u16,
    last_left_area: ratatui::layout::Rect,
    last_right_area: ratatui::layout::Rect,

    // Input history for up/down recall (glm.md UX)
    input_history: Vec<String>,
    history_index: Option<usize>,

    // Conversation / trace search
    search: SearchState,
    search_mode: bool,

    // Cached status-bar labels to avoid flicker when agent lock is contended (glm.md)
    cached_mode_label: String,
    cached_goal_text: String,
}

impl App {
    fn new(config: &Config) -> Self {
        let banner = format!(
            "Raven Hotel - Agent Harness\n\n\
             Endpoint: {}\n\
             Model:    {}\n\
             Workspace: {}\n\n\
             Session context, goal tracking, and a safe repo cache (tree + importance + recent summary)\n\
             are now persisted under ~/.raven-hotel/ and injected on every turn.\n\
             The model can call update_goal(...) and record_discovery(...) when intent shifts.\n\
             Type / in the input for commands (help, clear, reset, status...).\n\
             Use Ctrl-C to quit.",
            config.base_url,
            config.model,
            config.workspace.display()
        );
        Self {
            left_committed: vec![banner],
            current_response: String::new(),
            trace_lines: vec![],
            current_thinking: String::new(),
            input: String::new(),
            left_scroll: 0,
            right_scroll: 0,
            left_follow_output: true,
            right_follow_output: true,
            focused_pane: Pane::Left,
            scroll_flash_timer: 0,
            is_processing: false,
            spinner_tick: 0,
            tool_calls_this_turn: 0,
            turn_rounds: 0,
            ctx_used_tokens: 0,
            pending_approval: None,
            approval_responder: None,
            needs_redraw: true,
            mode_menu_active: false,
            selected_mode_idx: 0,
            approval_modes: [
                "Babysitter - Always Ask",
                "Spring Break - Yolo for remainder of session",
                "Vegas - Yolo in sandbox",
                "Thunderdome - eternal Yolo, anytime, anywhere",
            ],
            slash_commands: default_slash_commands(),
            slash_selected: 0,
            display_model: config.model.clone(),
            display_budget: config.context_budget.clone(),
            settings: SettingsModal::inactive(),
            last_left_line_count: 0,
            last_right_line_count: 0,
            last_left_area: ratatui::layout::Rect::default(),
            last_right_area: ratatui::layout::Rect::default(),
            input_history: vec![],
            history_index: None,
            search: SearchState::default(),
            search_mode: false,
            cached_mode_label: String::new(),
            cached_goal_text: "(no goal set)".into(),
        }
    }
}

fn render_pane(pane: Pane) -> crate::tui_render::Pane {
    match pane {
        Pane::Left => crate::tui_render::Pane::Left,
        Pane::Right => crate::tui_render::Pane::Right,
    }
}

async fn apply_settings_key(
    app: &mut App,
    key: crossterm::event::KeyEvent,
    config: &Config,
    keystore: &mut crate::keystore::Keystore,
    agent: &Arc<tokio::sync::Mutex<Agent>>,
) {
    let result = handle_settings_key(&mut app.settings, key, config, keystore, agent).await;
    apply_settings_actions(
        result.actions,
        &mut app.left_committed,
        &mut app.trace_lines,
        &mut app.display_model,
        &mut app.display_budget,
        &mut app.settings,
    );
    app.left_follow_output = true;
    app.left_scroll = 10_000;
    app.right_follow_output = true;
    app.right_scroll = 10_000;
    app.needs_redraw = true;
}

pub async fn run(
    config: Config,
    saved_endpoints: Vec<crate::config::InferenceEndpoint>,
    keystore: crate::keystore::Keystore,
) -> Result<()> {
    // Setup terminal cleanly.
    // We enter the alternate screen + raw mode with an explicit clear so that
    // any previous output (cargo warnings, shell history, etc.) does not show
    // "under" the TUI.
    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(
        stdout,
        crossterm::terminal::EnterAlternateScreen,
        crossterm::event::EnableMouseCapture,
        crossterm::terminal::Clear(crossterm::terminal::ClearType::All),
    )?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // One extra clear via the terminal API (belt + suspenders)
    terminal.clear()?;

    let res = run_app(&mut terminal, config, saved_endpoints, keystore).await;

    // Restore terminal
    crossterm::terminal::disable_raw_mode()?;
    crossterm::execute!(
        terminal.backend_mut(),
        crossterm::terminal::LeaveAlternateScreen,
        crossterm::event::DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    if let Err(err) = res {
        eprintln!("TUI error: {:?}", err);
    }
    Ok(())
}

async fn run_app<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    config: Config,
    saved_endpoints: Vec<crate::config::InferenceEndpoint>,
    mut keystore: crate::keystore::Keystore,
) -> Result<()> {
    // Wrap agent in Arc<Mutex> so it persists across spawned turn tasks.
    // Previously each turn moved the agent into the task and recreated a blank
    // one afterward, losing all conversation history.
    let agent = Arc::new(tokio::sync::Mutex::new(Agent::new(config.clone())));

    let mut app = App::new(&config);

    // Channel for agent to push live updates into the TUI
    let (tx, mut rx) = mpsc::channel::<UiUpdate>(64);

    // Channel for input thread to send key events to main loop
    let (input_tx, mut input_rx) = mpsc::channel::<Event>(64);

    // Channel for execution approval requests (tool needs user OK before running)
    // Sender goes to agent task, receiver stays in UI loop.
    // Carries (description, responder) so UI can show dialog and respond with bool.
    let (approval_req_tx, mut approval_req_rx) = mpsc::channel::<(String, tokio::sync::oneshot::Sender<bool>)>(4);

    // Stop signal: UI sets this to true (Escape while processing), agent task checks it
    // at clean stopping points (before each LLM call, before each tool execution).
    let stop_signal = Arc::new(AtomicBool::new(false));

    // Spawn a dedicated thread for keyboard input (responsive even during LLM streaming).
    // Uses blocking_send() to bridge the sync thread → async tokio channel.
    let _input_handle = std::thread::spawn(move || {
        loop {
            if event::poll(Duration::from_millis(10)).unwrap_or(false) {
                if let Ok(ev) = event::read() {
                    if input_tx.blocking_send(ev).is_err() {
                        break; // Main loop dropped receiver, exit thread
                    }
                }
            }
        }
    });

    loop {
        // Advance spinner
        if app.is_processing {
           app.spinner_tick = app.spinner_tick.wrapping_add(1);
        }

        // Read current state from agent (try_lock to avoid blocking the draw).
        // app.ctx_used_tokens is NOT read here — it's pushed via UiUpdate::ContextUsage
        // from the agent task to avoid any lock contention during processing.
        // On lock contention, reuse cached labels instead of showing "…" to avoid flicker.
        let (mode_label, goal_text) = if let Ok(ag) = agent.try_lock() {
            let mode = ag.current_exec_mode().label().to_string();
            let goal = ag.session.as_ref()
                .and_then(|s| {
                    let g = s.meta.current_goal.as_str();
                    if g.is_empty() { None } else { Some(g.to_string()) }
                })
                .unwrap_or_else(|| "(no goal set)".into());
            (mode, goal)
        } else {
            (app.cached_mode_label.clone(), app.cached_goal_text.clone())
        };
        app.cached_mode_label = mode_label.clone();
        app.cached_goal_text = goal_text.clone();

        // Draw only when needed (basic perf improvement per glm.md)
        if app.needs_redraw || app.is_processing || app.scroll_flash_timer > 0 {
           app.needs_redraw = false;
            // Draw - split into status bar + content area + context gauge + input bar
            terminal.draw(|f| {
            let size = f.area();

            // Vertical layout: status bar (1) + content panes (fill) + context gauge (1) + input bar (3)
            let show_gauge = app.is_processing;
            let gauge_h = if show_gauge { 1 } else { 0 };
            let vertical = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(1),        // status bar
                    Constraint::Min(6),           // content panes
                    Constraint::Length(gauge_h),   // context gauge (only during processing)
                    Constraint::Length(3),         // input bar
                ])
                .split(size);

            let status_area = vertical[0];
            let content_area = vertical[1];
            let gauge_area = vertical[2];
            let input_area = vertical[3];

            // ═══════════════════ STATUS BAR ═══════════════════
            let search_label = app.search.status_label();
            crate::tui_render::draw_status_bar(f, status_area, &crate::tui_render::StatusBarData {
                display_model: &app.display_model,
                ctx_used_tokens: app.ctx_used_tokens,
                budget: &app.display_budget,
                mode_label: &mode_label,
                goal_text: &goal_text,
                search_label: &search_label,
            });

            // ═══════════════════ CONTENT PANES ═══════════════════
            let panes = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(62), Constraint::Percentage(38)])
                .split(content_area);

            let left_area = panes[0];
            let right_area = panes[1];
           app.last_left_area = left_area;
           app.last_right_area = right_area;

            // Search highlight: only highlight on the pane that owns the search.
            let left_highlight = if app.search.active
                && app.search.pane == crate::tui_render::Pane::Left
                && !app.search.match_lines.is_empty()
            {
                Some(app.search.match_lines[app.search.match_idx])
            } else {
                None
            };
            let right_highlight = if app.search.active
                && app.search.pane == crate::tui_render::Pane::Right
                && !app.search.match_lines.is_empty()
            {
                Some(app.search.match_lines[app.search.match_idx])
            } else {
                None
            };

            // Call extracted (split rendering)
            crate::tui_render::draw_left_pane(
                f,
                &app.left_committed,
                &app.current_response,
                left_area,
                &mut app.last_left_area,
                &mut app.last_left_line_count,
                app.left_follow_output,
                &mut app.left_scroll,
                render_pane(app.focused_pane),
                app.scroll_flash_timer,
                left_highlight,
            );

            crate::tui_render::draw_right_pane(
                f,
                &app.trace_lines,
                &app.current_thinking,
                right_area,
                &mut app.last_right_area,
                &mut app.last_right_line_count,
                app.right_follow_output,
                &mut app.right_scroll,
                render_pane(app.focused_pane),
                app.scroll_flash_timer,
                right_highlight,
            );

            // ═══════════════════ CONTEXT GAUGE ═══════════════════
            if show_gauge {
                crate::tui_render::draw_context_gauge(f, gauge_area, &crate::tui_render::ContextGaugeData {
                    turn_rounds: app.turn_rounds,
                    max_rounds: config.max_rounds,
                    tool_calls_this_turn: app.tool_calls_this_turn,
                });
            }

            // ═══════════════════ INPUT BAR ═══════════════════
            crate::tui_render::draw_input_bar(f, input_area, &crate::tui_render::InputBarData {
                input: &app.input,
                is_processing: app.is_processing,
                spinner_tick: app.spinner_tick,
                search_mode: app.search_mode,
            });

            // Overlays: approval popup, slash menu, mode menu, settings modal
            crate::tui_render::draw_overlays(
                f,
                size,
                input_area,
                &app.settings,
                app.pending_approval.as_deref(),
                &app.slash_commands,
                &app.input,
                app.slash_selected,
                app.mode_menu_active,
                &app.approval_modes,
                app.selected_mode_idx,
            );

            // Cursor in input
            f.set_cursor_position((input_area.x + 1 + app.input.len() as u16, input_area.y + 1));
            
            // Decrement scroll flash timer
            if app.scroll_flash_timer > 0 {
              app.scroll_flash_timer = app.scroll_flash_timer.saturating_sub(1);
            }
        })?;
        }

        // Poll for new approval requests from the agent background task
        while let Ok((desc, tx)) = approval_req_rx.try_recv() {
           app.pending_approval = Some(desc.clone());
           app.approval_responder = Some(tx);
           app.needs_redraw = true; // force a redraw so popup shows
           app.input.clear(); // don't leave garbage in the input field
           app.trace_lines.push(format!("🔒 Approval requested for: {}", desc));
           app.left_committed.push(format!("🔒 Approval requested for: {}", desc));
           app.left_follow_output = true;
           app.left_scroll = 10_000;
           app.right_follow_output = true;
           app.right_scroll = 10_000;
        }
        // If we just picked up a NEW approval this iteration, force a redraw
        // so the popup is visible before we block on select! waiting for input.
        // The `app.needs_redraw` flag prevents a hot loop (only continue once).
        if app.needs_redraw {
           app.needs_redraw = false;
            continue; // → top of loop → draw → select
        }

        // Handle input + agent updates (non-blocking)
        if app.is_processing {
            // While processing we mostly listen for agent updates and a few keys
            tokio::select! {
                Some(update) = rx.recv() => {
                    // Always poll for approval requests on any agent update to ensure dialog shows promptly
                    while let Ok((desc, tx)) = approval_req_rx.try_recv() {
                       app.pending_approval = Some(desc.clone());
                       app.approval_responder = Some(tx);
                       app.input.clear();
                      app.trace_lines.push(format!("🔒 Approval requested for: {}", desc));
                       app.left_committed.push(format!("🔒 Approval requested for: {}", desc));
                      app.left_follow_output = true;
                      app.left_scroll = 10_000;
                      app.right_follow_output = true;
                      app.right_scroll = 10_000;
                    }
                    match update {
                        UiUpdate::Token(t) => {
                            // Regular content tokens → live on the LEFT pane (current turn output)
                           app.current_response.push_str(&t);
                           app.needs_redraw = true;
                          app.left_follow_output = true;
                          app.left_scroll = 10_000; // auto-scroll to bottom while streaming output
                        }
                        UiUpdate::Thinking(t) => {
                            // Accumulate small thinking chunks (models often send 1-3 tokens at a time)
                            // and only commit to app.trace_lines on reasonable boundaries.
                          app.current_thinking.push_str(&t);
                           app.needs_redraw = true;
                          app.right_follow_output = true;
                          app.right_scroll = 10_000; // auto-scroll trace pane on new thinking

                            // Flush heuristic: paragraph break, sentence terminator + space, or size limit.
                            // This turns "one word per line" into proper sentences/paragraphs in the trace.
                            let should_flush =
                              app.current_thinking.contains("\n\n") ||
                              app.current_thinking.ends_with(". ") ||
                              app.current_thinking.ends_with("! ") ||
                              app.current_thinking.ends_with("? ") ||
                              app.current_thinking.len() > 160;

                            if should_flush {
                                let block = app.current_thinking.trim().to_string();
                                if !block.is_empty() {
                                  app.trace_lines.push(format!("🧠 {}", block));
                                  app.right_follow_output = true;
                                  app.right_scroll = 10_000;
                                }
                              app.current_thinking.clear();
                            }
                        }
                        UiUpdate::ToolStart { name, args } => {
                            // Tool activity → RIGHT pane (debug)
                          app.trace_lines.push(format!("🔧 {}({})", name, truncate(&args, 90)));
                           app.tool_calls_this_turn += 1;
                          app.right_follow_output = true;
                          app.right_scroll = 10_000;
                        }
                        UiUpdate::ToolResult { name, summary } => {
                          app.trace_lines.push(format!("   ↳ {} → {}", name, truncate(&summary, 120)));
                          app.right_follow_output = true;
                          app.right_scroll = 10_000;
                        }
                        UiUpdate::RoundLimitHit { continuation, max_continuations, exhausted } => {
                            let msg = if exhausted {
                                format!(
                                    "⏸ Round limit — exhausted auto-continue budget ({}/{}). Send another message to continue.",
                                    continuation, max_continuations
                                )
                            } else {
                                format!(
                                    "⟳ Round limit hit — auto-continuing ({}/{})...",
                                    continuation, max_continuations
                                )
                            };
                          app.trace_lines.push(msg);
                           app.turn_rounds += 1;
                          app.right_follow_output = true;
                          app.right_scroll = 10_000;
                        }
                        UiUpdate::Done { final_text } => {
                            // Turn complete. Flush the output.
                            // Use the Done's final_text as a robust fallback in case no individual
                            // Token updates were emitted by the stream (some llama.cpp configurations
                            // deliver the full response only in the final payload).
                            let output = if !app.current_response.trim().is_empty() {
                                app.current_response.trim().to_string()
                            } else if !final_text.trim().is_empty() {
                                final_text
                            } else if !app.current_thinking.trim().is_empty() {
                                // Fallback: if the model delivered the response via the reasoning/thinking channel
                                // (no regular content tokens), use it so output appears in the conversation pane.
                                app.current_thinking.trim().to_string()
                            } else {
                                String::new()
                            };
                            if !output.is_empty() {
                               app.left_committed.push(format!("Agent: {}", output));
                                #[cfg(feature = "clipboard")]
                                try_copy_to_clipboard(&output);
                              app.left_follow_output = true;
                              app.left_scroll = 10_000; // auto-scroll to bottom after agent response
                               app.current_response.clear();
                            }
                            // Flush any remaining live thinking (to right trace pane)
                            if !app.current_thinking.trim().is_empty() {
                              app.trace_lines.push(format!("🧠 {}", app.current_thinking.trim()));
                              app.current_thinking.clear();
                            }
                            if !app.trace_lines.is_empty() {
                              app.right_follow_output = true;
                              app.right_scroll = 10_000;
                            }
                          app.needs_redraw = true;
                          app.is_processing = false;
                        }
                        UiUpdate::Error(e) => {
                            let msg = format!("⚠ ERROR: {}", e);
                          app.trace_lines.push(msg.clone());
                          app.right_follow_output = true;
                          app.right_scroll = 10_000;
                            // Flush any pending thinking on error
                            if !app.current_thinking.trim().is_empty() {
                              app.trace_lines.push(format!("🧠 {}", app.current_thinking.trim()));
                              app.current_thinking.clear();
                            }
                            // Make errors visible in the main left pane too
                           app.left_committed.push(msg);
                            if !app.current_response.trim().is_empty() {
                               app.left_committed.push(format!("Agent (partial): {}", app.current_response.trim()));
                               app.current_response.clear();
                            }
                          app.needs_redraw = true;
                          app.is_processing = false;
                        }
                        UiUpdate::ApprovalRequested => {
                            // Poll the approval channel here to set pending immediately
                            while let Ok((desc, tx)) = approval_req_rx.try_recv() {
                               app.pending_approval = Some(desc);
                               app.approval_responder = Some(tx);
                               app.needs_redraw = true;
                               app.input.clear();
                            }
                            // Force a redraw so the dialog appears immediately
                          app.left_follow_output = true;
                          app.right_follow_output = true;
                        }
                        UiUpdate::ContextUsage { used_tokens } => {
                          app.ctx_used_tokens = used_tokens;
                        }
                    }
                }

                // Allow the user to scroll AND type-ahead while processing
                Some(ev) = input_rx.recv() => {
                    if let Event::Key(key) = ev {
                        // Highest priority: approval dialog
                        if app.pending_approval.is_some() {
                            match key.code {
                                KeyCode::Char('y') | KeyCode::Char('Y') => {
                                    if let Some(tx) = app.approval_responder.take() {
                                        let _ = tx.send(true);
                                    }
                                   app.pending_approval = None;
                                   app.left_committed.push("✅ Action approved".to_string());
                                  app.left_follow_output = true;
                                  app.left_scroll = 10_000;
                                }
                                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                                    if let Some(tx) = app.approval_responder.take() {
                                        let _ = tx.send(false);
                                    }
                                   app.pending_approval = None;
                                   app.left_committed.push("⛔ Action denied".to_string());
                                  app.left_follow_output = true;
                                  app.left_scroll = 10_000;
                                }
                                _ => {}
                            }
                            continue;
                        }

                        if app.settings.active {
                            apply_settings_key(&mut app, key, &config, &mut keystore, &agent).await;
                            continue;
                        }

                        // Handle /mode selection menu first if active
                        if app.mode_menu_active {
                            match key.code {
                                KeyCode::Up | KeyCode::Char('k') => { if app.selected_mode_idx > 0 { app.selected_mode_idx -= 1; } app.needs_redraw = true; }
                                KeyCode::Down | KeyCode::Char('j') => { if app.selected_mode_idx < 3 { app.selected_mode_idx += 1; } app.needs_redraw = true; }
                                KeyCode::Enter => {
                                    let chosen = app.approval_modes[app.selected_mode_idx];
                                   app.left_committed.push(format!("Execution mode set to: {}", chosen));
                                  app.left_follow_output = true;
                                  app.left_scroll = 10_000;

                                    match app.selected_mode_idx {
                                        0 => { if let Ok(mut ag) = agent.try_lock() { ag.set_exec_approval_mode(crate::session::ExecApprovalMode::Babysitter); if let Some(s) = &mut ag.session { let _ = s.save_meta(); } } }
                                        1 => { if let Ok(mut ag) = agent.try_lock() { ag.set_exec_approval_mode(crate::session::ExecApprovalMode::SpringBreak); if let Some(s) = &mut ag.session { let _ = s.save_meta(); } } }
                                        2 => { if let Ok(mut ag) = agent.try_lock() { ag.set_exec_approval_mode(crate::session::ExecApprovalMode::Vegas); if let Some(s) = &mut ag.session { let _ = s.save_meta(); } } }
                                        3 => { if let Ok(mut ag) = agent.try_lock() { ag.set_exec_approval_mode(crate::session::ExecApprovalMode::Thunderdome); if let Some(s) = &mut ag.session { let _ = s.save_meta(); } } }
                                        _ => {}
                                    }
                                   app.mode_menu_active = false;
                                   app.input.clear();
                                   app.selected_mode_idx = 0;
                                   app.needs_redraw = true;
                                }
                                KeyCode::Esc => {
                                   app.mode_menu_active = false;
                                   app.input.clear();
                                   app.selected_mode_idx = 0;
                                   app.needs_redraw = true;
                                }
                                _ => {}
                            }
                            continue;
                        }

                        match key.code {
                            KeyCode::Tab => {
                              app.focused_pane = match app.focused_pane {
                                    Pane::Left => Pane::Right,
                                    Pane::Right => Pane::Left,
                                };
                                continue; // Force redraw to show focus change
                            }
                            KeyCode::Esc => {
                                // STOP: signal the agent task to halt at the next clean point
                                stop_signal.store(true, Ordering::SeqCst);
                                // If there's a pending approval, deny it automatically
                                if let Some(tx) = app.approval_responder.take() {
                                    let _ = tx.send(false);
                                }
                               app.pending_approval = None;
                              app.trace_lines.push("⏹ Stop requested by user (Esc)".to_string());
                               app.left_committed.push("⏹ Stopping agent...".to_string());
                              app.left_follow_output = true;
                              app.left_scroll = 10_000;
                              app.right_follow_output = true;
                              app.right_scroll = 10_000;
                            }

                            KeyCode::PageUp if app.focused_pane == Pane::Right => { app.right_follow_output = false; let old_scroll = app.right_scroll; app.right_scroll = app.right_scroll.saturating_sub(8); if old_scroll == app.right_scroll && old_scroll == 0 { app.scroll_flash_timer = 10; } }
                            KeyCode::PageUp if app.focused_pane == Pane::Left => { app.left_follow_output = false; let old_scroll = app.left_scroll; app.left_scroll = app.left_scroll.saturating_sub(8); if old_scroll == app.left_scroll && old_scroll == 0 { app.scroll_flash_timer = 10; } }
                            KeyCode::PageDown if app.focused_pane == Pane::Right => { let old_scroll = app.right_scroll; app.right_scroll = app.right_scroll.saturating_add(8); let right_max = app.last_right_line_count.saturating_sub(app.last_right_area.height.saturating_sub(2)); if old_scroll == app.right_scroll && old_scroll >= right_max { app.scroll_flash_timer = 10; } }
                            KeyCode::PageDown if app.focused_pane == Pane::Left => { let old_scroll = app.left_scroll; app.left_scroll = app.left_scroll.saturating_add(8); let left_max = app.last_left_line_count.saturating_sub(app.last_left_area.height.saturating_sub(2)); if old_scroll == app.left_scroll && old_scroll >= left_max { app.scroll_flash_timer = 10; } }
                            KeyCode::Up if app.focused_pane == Pane::Right => { app.right_follow_output = false; let old_scroll = app.right_scroll; app.right_scroll = app.right_scroll.saturating_sub(1); if old_scroll == app.right_scroll && old_scroll == 0 { app.scroll_flash_timer = 10; } }
                            KeyCode::Up if app.focused_pane == Pane::Left => { app.left_follow_output = false; let old_scroll = app.left_scroll; app.left_scroll = app.left_scroll.saturating_sub(1); if old_scroll == app.left_scroll && old_scroll == 0 { app.scroll_flash_timer = 10; } }
                            KeyCode::Down if app.focused_pane == Pane::Right => { let old_scroll = app.right_scroll; app.right_scroll = app.right_scroll.saturating_add(1); let right_max = app.last_right_line_count.saturating_sub(app.last_right_area.height.saturating_sub(2)); if old_scroll == app.right_scroll && old_scroll >= right_max { app.scroll_flash_timer = 10; } }
                            KeyCode::Down if app.focused_pane == Pane::Left => { let old_scroll = app.left_scroll; app.left_scroll = app.left_scroll.saturating_add(1); let left_max = app.last_left_line_count.saturating_sub(app.last_left_area.height.saturating_sub(2)); if old_scroll == app.left_scroll && old_scroll >= left_max { app.scroll_flash_timer = 10; } }
                            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                return Ok(());
                            }

                            // Allow typing ahead while the model is working
                            KeyCode::Char(c) => { app.input.push(c); app.history_index = None; clamp_slash_selection(&app.slash_commands, &app.input, &mut app.slash_selected); app.needs_redraw = true; }
                            KeyCode::Backspace => { app.input.pop(); app.history_index = None; clamp_slash_selection(&app.slash_commands, &app.input, &mut app.slash_selected); app.needs_redraw = true; }
                            _ => {}
                        }
                    }
                }

                else => {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }
            continue;
        }

        // Normal input handling when idle — read from the input channel
        match tokio::time::timeout(Duration::from_millis(50), input_rx.recv()).await {
            Ok(Some(Event::Key(key))) => {
                if app.pending_approval.is_some() {
                    match key.code {
                        KeyCode::Char('y') | KeyCode::Char('Y') => {
                            if let Some(tx) = app.approval_responder.take() {
                                let _ = tx.send(true);
                            }
                           app.pending_approval = None;
                           app.left_committed.push("✅ Action approved".to_string());
                          app.left_follow_output = true;
                          app.left_scroll = 10_000;
                        }
                        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                            if let Some(tx) = app.approval_responder.take() {
                                let _ = tx.send(false);
                            }
                           app.pending_approval = None;
                           app.left_committed.push("⛔ Action denied".to_string());
                          app.left_follow_output = true;
                          app.left_scroll = 10_000;
                        }
                        _ => {}
                    }
                    // fallthrough to match below (input guards prevent typing; Enter dispatch will see cleared input)
                } else if app.mode_menu_active {
                    match key.code {
                        KeyCode::Up | KeyCode::Char('k') => { if app.selected_mode_idx > 0 { app.selected_mode_idx -= 1; } app.needs_redraw = true; }
                        KeyCode::Down | KeyCode::Char('j') => { if app.selected_mode_idx < 3 { app.selected_mode_idx += 1; } app.needs_redraw = true; }
                        KeyCode::Enter => {
                            let chosen = app.approval_modes[app.selected_mode_idx];
                           app.left_committed.push(format!("Execution mode set to: {}", chosen));
                          app.left_follow_output = true;
                          app.left_scroll = 10_000;

                            match app.selected_mode_idx {
                                0 => { if let Ok(mut ag) = agent.try_lock() { ag.set_exec_approval_mode(crate::session::ExecApprovalMode::Babysitter); if let Some(s) = &mut ag.session { let _ = s.save_meta(); } } }
                                1 => { if let Ok(mut ag) = agent.try_lock() { ag.set_exec_approval_mode(crate::session::ExecApprovalMode::SpringBreak); if let Some(s) = &mut ag.session { let _ = s.save_meta(); } } }
                                2 => { if let Ok(mut ag) = agent.try_lock() { ag.set_exec_approval_mode(crate::session::ExecApprovalMode::Vegas); if let Some(s) = &mut ag.session { let _ = s.save_meta(); } } }
                                3 => { if let Ok(mut ag) = agent.try_lock() { ag.set_exec_approval_mode(crate::session::ExecApprovalMode::Thunderdome); if let Some(s) = &mut ag.session { let _ = s.save_meta(); } } }
                                _ => {}
                            }
                           app.mode_menu_active = false;
                           app.input.clear();
                           app.selected_mode_idx = 0;
                           app.needs_redraw = true;
                        }
                        KeyCode::Esc => {
                           app.mode_menu_active = false;
                           app.input.clear();
                           app.selected_mode_idx = 0;
                           app.needs_redraw = true;
                        }
                        _ => {}
                    }
                    // fallthrough; big match below has guards so chars etc don't mutate input while menu is open
                } else if app.settings.active {
                    apply_settings_key(&mut app, key, &config, &mut keystore, &agent).await;
                }
                if !app.settings.active {
                match key.code {
                    // --- Slash command menu navigation (takes precedence when active) ---
                    KeyCode::Up if app.input.starts_with('/') => {
                        let filtered = get_filtered_commands(&app.slash_commands, &app.input);
                        if !filtered.is_empty() {
                           app.slash_selected = app.slash_selected.saturating_sub(1);
                        }
                       app.needs_redraw = true;
                    }
                    KeyCode::Down if app.input.starts_with('/') => {
                        let filtered = get_filtered_commands(&app.slash_commands, &app.input);
                        if !filtered.is_empty() {
                           app.slash_selected = (app.slash_selected + 1).min(filtered.len().saturating_sub(1));
                        }
                       app.needs_redraw = true;
                    }
                    KeyCode::Tab if app.input.starts_with('/') => {
                        let filtered = get_filtered_commands(&app.slash_commands, &app.input);
                        if let Some(cmd) = filtered.get(app.slash_selected.min(filtered.len().saturating_sub(1))) {
                           app.input = format!("/{} ", cmd.name);
                            clamp_slash_selection(&app.slash_commands, &app.input, &mut app.slash_selected);
                        }
                    }

                    KeyCode::Tab => {
                      app.focused_pane = match app.focused_pane {
                            Pane::Left => Pane::Right,
                            Pane::Right => Pane::Left,
                        };
                    }
                    KeyCode::Esc => {
                        if app.input.starts_with('/') {
                            // Dismiss command menu / clear partial command
                           app.input.clear();
                           app.slash_selected = 0;
                        } else {
                            // Release focus on escape
                          app.focused_pane = Pane::Left;
                          app.left_follow_output = true;
                          app.right_follow_output = true;
                        }
                    }
                    KeyCode::PageUp if app.focused_pane == Pane::Right => { app.right_follow_output = false; let old_scroll = app.right_scroll; app.right_scroll = app.right_scroll.saturating_sub(8); if old_scroll == app.right_scroll && old_scroll == 0 { app.scroll_flash_timer = 10; } }
                    KeyCode::PageUp if app.focused_pane == Pane::Left => { app.left_follow_output = false; let old_scroll = app.left_scroll; app.left_scroll = app.left_scroll.saturating_sub(8); if old_scroll == app.left_scroll && old_scroll == 0 { app.scroll_flash_timer = 10; } }
                    KeyCode::PageDown if app.focused_pane == Pane::Right => { let old_scroll = app.right_scroll; app.right_scroll = app.right_scroll.saturating_add(8); let right_max = app.last_right_line_count.saturating_sub(app.last_right_area.height.saturating_sub(2)); if old_scroll == app.right_scroll && old_scroll >= right_max { app.scroll_flash_timer = 10; } }
                    KeyCode::PageDown if app.focused_pane == Pane::Left => { let old_scroll = app.left_scroll; app.left_scroll = app.left_scroll.saturating_add(8); let left_max = app.last_left_line_count.saturating_sub(app.last_left_area.height.saturating_sub(2)); if old_scroll == app.left_scroll && old_scroll >= left_max { app.scroll_flash_timer = 10; } }
                    KeyCode::Up if !app.mode_menu_active && app.focused_pane == Pane::Right => { app.right_follow_output = false; let old_scroll = app.right_scroll; app.right_scroll = app.right_scroll.saturating_sub(1); if old_scroll == app.right_scroll && old_scroll == 0 { app.scroll_flash_timer = 10; } }
                    KeyCode::Up if !app.mode_menu_active && app.focused_pane == Pane::Left => { app.left_follow_output = false; let old_scroll = app.left_scroll; app.left_scroll = app.left_scroll.saturating_sub(1); if old_scroll == app.left_scroll && old_scroll == 0 { app.scroll_flash_timer = 10; } }
                    KeyCode::Down if !app.mode_menu_active && app.focused_pane == Pane::Right => { let old_scroll = app.right_scroll; app.right_scroll = app.right_scroll.saturating_add(1); let right_max = app.last_right_line_count.saturating_sub(app.last_right_area.height.saturating_sub(2)); if old_scroll == app.right_scroll && old_scroll >= right_max { app.scroll_flash_timer = 10; } }
                    KeyCode::Down if !app.mode_menu_active && app.focused_pane == Pane::Left => { let old_scroll = app.left_scroll; app.left_scroll = app.left_scroll.saturating_add(1); let left_max = app.last_left_line_count.saturating_sub(app.last_left_area.height.saturating_sub(2)); if old_scroll == app.left_scroll && old_scroll >= left_max { app.scroll_flash_timer = 10; } }
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        return Ok(());
                    }
                    KeyCode::Enter => {
                        let prompt = app.input.trim().to_string();
                        if !prompt.is_empty() {
                            if app.input_history.last() != Some(&prompt) {
                                app.input_history.push(prompt.clone());
                            }
                            app.history_index = None;
                        }
                        if prompt.is_empty() {
                            // nothing to do
                        } else if prompt.starts_with('/') {
                            let mut slash_ctx = SlashContext {
                                left_committed: &mut app.left_committed,
                                trace_lines: &mut app.trace_lines,
                                current_response: &app.current_response,
                                current_thinking: &app.current_thinking,
                                input: &mut app.input,
                                slash_commands: &app.slash_commands,
                                slash_selected: &mut app.slash_selected,
                                mode_menu_active: &mut app.mode_menu_active,
                                selected_mode_idx: &mut app.selected_mode_idx,
                                settings: &mut app.settings,
                                search: &mut app.search,
                                focused_pane: render_pane(app.focused_pane),
                                left_scroll: &mut app.left_scroll,
                                right_scroll: &mut app.right_scroll,
                                left_follow_output: &mut app.left_follow_output,
                                right_follow_output: &mut app.right_follow_output,
                                last_left_line_count: app.last_left_line_count,
                                last_right_line_count: app.last_right_line_count,
                                last_left_area_h: app.last_left_area.height,
                                last_right_area_h: app.last_right_area.height,
                                config: &config,
                                saved_endpoints: &saved_endpoints,
                                agent: &agent,
                            };
                            match dispatch_slash_command(&prompt, &mut slash_ctx) {
                                SlashDispatch::Quit => return Ok(()),
                                SlashDispatch::Handled => {
                                    // /search sets pane scroll via apply_search_scroll; don't jump to bottom.
                                    if app.search.active
                                        && !app.search.query.is_empty()
                                        && !app.search.match_lines.is_empty()
                                    {
                                        app.needs_redraw = true;
                                    } else {
                                        app.left_follow_output = true;
                                        app.left_scroll = 10_000;
                                        app.needs_redraw = true;
                                    }
                                }
                                SlashDispatch::AgentPrompt(()) => {}
                            }
                        } else {
                            // === Normal user prompt to the agent ===
                            // Commit any previous live response (from last turn) if present
                            // Guard against duplicate if Done already committed it.
                            if !app.current_response.trim().is_empty() {
                                let agent_msg = format!("Agent: {}", app.current_response.trim());
                                if app.left_committed.last() != Some(&agent_msg) {
                                    app.left_committed.push(agent_msg);
                                }
                            }

                            // New turn: record user prompt on left, clear live + trace for fresh view
                           app.left_committed.push(format!("You: {}", prompt));
                          app.left_follow_output = true;
                          app.left_scroll = 10_000; // auto-scroll to bottom on new user message
                           app.current_response.clear();
                          app.trace_lines.clear();
                          app.current_thinking.clear();
                          app.trace_lines.push(format!("▶ Starting agent turn for: {}", prompt));
                          app.trace_lines.push("   (waiting for first response from model...)".to_string());
                          app.right_follow_output = true;
                          app.right_scroll = 10_000; // auto-scroll trace pane on new turn start

                           app.input.clear();
                           app.slash_selected = 0;
                          app.is_processing = true;
                           app.tool_calls_this_turn = 0;
                           app.turn_rounds = 0;

                            // Spawn the agent turn — agent is behind Arc<Mutex> so it
                            // persists across turns with full conversation history.
                            let tx2 = tx.clone();
                            let agent_clone = agent.clone();
                            let prompt2 = prompt.clone();
                            let max_rounds = config.max_rounds.min(12);

                            let approval_req_tx2 = approval_req_tx.clone();
                            let stop = stop_signal.clone();
                            stop.store(false, Ordering::SeqCst); // reset for new turn
                            tokio::spawn(async move {
                                let mut agent = agent_clone.lock().await;

                                let mode = agent.current_exec_mode();
                                let _ = tx2.send(UiUpdate::ToolResult {
                                    name: "system".into(),
                                    summary: format!("Turn started with exec mode: {:?}", mode),
                                }).await;

                                // Start the first streaming inference (adds user message to history)
                                let first_stream = match agent.run_turn_streaming(&prompt2).await {
                                    Ok(s) => s,
                                    Err(e) => {
                                        let _ = tx2.send(UiUpdate::Error(e.to_string())).await;
                                        return;
                                    }
                                };

                                let mut current_stream = first_stream;
                                let mut final_text = String::new();
                                let max_auto_continues: u32 = 3;
                                let max_text_nudges: u32 = 2;
                                let mut denials_this_turn: u32 = 0;
                                let mut text_nudges: u32 = 0;
                                let mut tools_used_this_turn: usize = 0;

                                // Outer loop: auto-continue when the model hits the round
                                // limit but was still actively calling tools.
                                'auto_continue: for continuation in 0..=max_auto_continues {
                                    let mut completed_naturally = false;

                                    // Inner loop: multi-round tool use within one budget.
                                    for _round in 0..max_rounds {
                                        let mut round_text = String::new();
                                        let mut tool_calls = vec![];

                                        // Consume the stream for this round
                                        while let Some(chunk) = current_stream.recv().await {
                                            // Check stop signal between chunks
                                            if stop.load(Ordering::SeqCst) {
                                                break;
                                            }
                                            match chunk {
                                                StreamChunk::Token(t) => {
                                                    round_text.push_str(&t);
                                                    let _ = tx2.send(UiUpdate::Token(t)).await;
                                                }
                                                StreamChunk::Thinking(t) => {
                                                    let _ = tx2.send(UiUpdate::Thinking(t)).await;
                                                }
                                                StreamChunk::Done { content, tool_calls: tcs, .. } => {
                                                    if !content.is_empty() && round_text.is_empty() {
                                                        round_text = content.clone();
                                                        let _ = tx2.send(UiUpdate::Token(content)).await;
                                                    }
                                                    tool_calls = tcs;
                                                }
                                                StreamChunk::Error(e) => {
                                                    let _ = tx2.send(UiUpdate::Error(e)).await;
                                                    return;
                                                }
                                            }
                                        }

                                        // Check if we broke out due to stop signal
                                        if stop.load(Ordering::SeqCst) {
                                            // Save any partial text we got
                                            if !round_text.trim().is_empty() {
                                                agent.push_assistant_text(&round_text);
                                            }
                                            let _ = tx2.send(UiUpdate::Done {
                                                final_text: round_text,
                                            }).await;
                                            return;
                                        }

                                        // Record assistant text in the conversation history
                                        if !round_text.trim().is_empty() {
                                            agent.push_assistant_text(&round_text);
                                            final_text = round_text;
                                        }

                                        // No tool calls → model stopped on its own.
                                        // But if tools were used this turn and we have
                                        // nudge budget, push it to keep working rather
                                         // than narrating.
                                        if tool_calls.is_empty() {
                                            if tools_used_this_turn > 0 && text_nudges < max_text_nudges {
                                                text_nudges += 1;
                                                let _ = tx2.send(UiUpdate::ToolResult {
                                                    name: "system".into(),
                                                    summary: format!("Nudging agent to continue (text-only pause {}/{})", text_nudges, max_text_nudges),
                                                }).await;
                                                // Push a continuation nudge as a user message
                                                agent.push_continuation_nudge();
                                                match agent.continue_turn_streaming().await {
                                                    Ok(s) => { current_stream = s; continue; }
                                                    Err(e) => {
                                                        let _ = tx2.send(UiUpdate::Error(e.to_string())).await;
                                                        return;
                                                    }
                                                }
                                            }
                                            completed_naturally = true;
                                            break;
                                        }
                                        tools_used_this_turn += tool_calls.len();

                                        // Check stop signal before executing any tools
                                        if stop.load(Ordering::SeqCst) {
                                            let _ = tx2.send(UiUpdate::Done {
                                                final_text: final_text.clone(),
                                            }).await;
                                            return;
                                        }

                                        // Execute tool calls (with possible approval dialog) and report to UI
                                        // We only actually execute the subset that are approved (or don't need approval).
                                        let mut to_execute: Vec<ToolCall> = vec![];

                                        for tc in &tool_calls {
                                            let current_mode = agent.current_exec_mode();

                                            let name = tc.function.name.as_str();
                                            let is_mutating = matches!(name, "write" | "patch" | "exec");

                                            let is_outside = if name == "exec" {
                                                let cmd = serde_json::from_str::<serde_json::Value>(&tc.function.arguments)
                                                    .ok()
                                                    .and_then(|v| v.get("command").and_then(|c| c.as_str()).map(|s| s.to_owned()))
                                                    .unwrap_or_default();
                                                // simple sandbox escape detection (cd / , absolute outside ws, network)
                                                cmd.contains("cd /") ||
                                                cmd.contains("/etc") || cmd.contains("/root") ||
                                                cmd.contains("curl ") || cmd.contains("wget ") || cmd.contains("nc ")
                                            } else {
                                                false
                                            };

                                            let needs = match current_mode {
                                                crate::session::ExecApprovalMode::Babysitter => is_mutating,
                                                crate::session::ExecApprovalMode::SpringBreak => false,
                                                crate::session::ExecApprovalMode::Vegas => name == "exec" && is_outside,
                                                crate::session::ExecApprovalMode::Thunderdome => false,
                                            };

                                            if needs {
                                                // Build a short, safe description (never dump full file content)
                                                let desc = match name {
                                                    "write" => {
                                                        let v: serde_json::Value = serde_json::from_str(&tc.function.arguments).unwrap_or_default();
                                                        let path = v.get("path").and_then(|p| p.as_str()).unwrap_or("?");
                                                        let n = v.get("content").and_then(|c| c.as_str()).map(|s| s.len()).unwrap_or(0);
                                                        format!("write {} ({} bytes)", path, n)
                                                    }
                                                    "patch" => {
                                                        let v: serde_json::Value = serde_json::from_str(&tc.function.arguments).unwrap_or_default();
                                                        let path = v.get("path").and_then(|p| p.as_str()).unwrap_or("?");
                                                        format!("patch {}", path)
                                                    }
                                                    "exec" => {
                                                        let v: serde_json::Value = serde_json::from_str(&tc.function.arguments).unwrap_or_default();
                                                        let cmd = v.get("command").and_then(|c| c.as_str()).unwrap_or("");
                                                        // truncate long commands for the dialog
                                                        let short = if cmd.len() > 120 { format!("{}...", &cmd[..120]) } else { cmd.to_string() };
                                                        format!("exec: {}", short)
                                                    }
                                                    "update_goal" => {
                                                        let goal = serde_json::from_str::<serde_json::Value>(&tc.function.arguments)
                                                            .ok()
                                                            .and_then(|v| v.get("goal").and_then(|g| g.as_str()).map(|s| s.to_string()))
                                                            .unwrap_or_else(|| tc.function.arguments.clone());
                                                        format!("update_goal: {}", if goal.len() > 80 { format!("{}...", &goal[..80]) } else { goal })
                                                    }
                                                    other => format!("{} (args omitted for display)", other),
                                                };

                                                let (resp_tx, resp_rx) = tokio::sync::oneshot::channel::<bool>();
                                                let _ = approval_req_tx2.send((desc.clone(), resp_tx)).await;
                                                let _ = tx2.send(UiUpdate::ApprovalRequested).await;  // wake UI to show dialog
                                                // UI will show dialog; block here until user answers
                                                match resp_rx.await {
                                                    Ok(true) => {
                                                        to_execute.push(tc.clone());
                                                        let _ = tx2.send(UiUpdate::ToolStart {
                                                            name: tc.function.name.clone(),
                                                            args: tc.function.arguments.clone(),
                                                        }).await;
                                                    }
                                                    _ => {
                                                        let deny = format!(
                                                            "DENIED: The user refused to approve this {} action. \
                                                             Do NOT retry the same action. \
                                                             Either try a different approach, ask the user what they want, \
                                                             or explain what you were trying to do and why.",
                                                            tc.function.name
                                                        );
                                                        denials_this_turn += 1;
                                                        let _ = tx2.send(UiUpdate::ToolResult {
                                                            name: tc.function.name.clone(),
                                                            summary: deny.to_string(),
                                                        }).await;
                                                        // Feed denial back to the model so the turn can continue consistently
                                                        agent.record_tool_denial(tc, &deny);
                                                        // After too many denials, stop the tool loop entirely
                                                        if denials_this_turn >= 3 {
                                                            let _ = tx2.send(UiUpdate::ToolResult {
                                                                name: "system".into(),
                                                                summary: "3 actions denied this turn — stopping tool loop. Send a new message to continue.".into(),
                                                            }).await;
                                                            break;
                                                        }
                                                        continue;
                                                    }
                                                }
                                            } else {
                                                to_execute.push(tc.clone());
                                                let _ = tx2.send(UiUpdate::ToolStart {
                                                    name: tc.function.name.clone(),
                                                    args: tc.function.arguments.clone(),
                                                }).await;
                                            }
                                        }

                                        let records = agent
                                            .execute_and_record_tool_calls(&to_execute)
                                            .await;

                                        for r in records {
                                            let _ = tx2.send(UiUpdate::ToolResult {
                                                name: r.tool,
                                                summary: r.summary,
                                            }).await;
                                        }

                                        // Push updated context usage to UI (lock-free)
                                        let _ = tx2.send(UiUpdate::ContextUsage {
                                            used_tokens: agent.estimated_context_tokens(),
                                        }).await;

                                        // Check stop signal before starting next inference
                                        if stop.load(Ordering::SeqCst) {
                                            let _ = tx2.send(UiUpdate::Done {
                                                final_text: final_text.clone(),
                                            }).await;
                                            return;
                                        }

                                        // Continue with another streaming inference
                                        match agent.continue_turn_streaming().await {
                                            Ok(s) => current_stream = s,
                                            Err(e) => {
                                                let _ = tx2.send(UiUpdate::Error(e.to_string())).await;
                                                return;
                                            }
                                        }
                                    }

                                    // Model stopped calling tools — we're done
                                    if completed_naturally {
                                        break 'auto_continue;
                                    }

                                    // Hit round limit while still calling tools.
                                    // The last iteration already created a pending stream
                                    // via continue_turn_streaming that hasn't been consumed.
                                    if continuation >= max_auto_continues {
                                        // Exhausted auto-continue budget
                                        let _ = tx2.send(UiUpdate::RoundLimitHit {
                                            continuation: continuation + 1,
                                            max_continuations: max_auto_continues + 1,
                                            exhausted: true,
                                        }).await;
                                        break 'auto_continue;
                                    }

                                    // Auto-continue: notify UI and loop back to consume
                                    // the pending stream with a fresh round budget.
                                    let _ = tx2.send(UiUpdate::RoundLimitHit {
                                        continuation: continuation + 1,
                                        max_continuations: max_auto_continues + 1,
                                        exhausted: false,
                                    }).await;

                                    // current_stream is already set from the last
                                    // continue_turn_streaming — just loop back.
                                }

                                // Flush session summary so it's fresh for the next restart
                                agent.force_flush_session().await;

                                // Final context usage snapshot
                                let _ = tx2.send(UiUpdate::ContextUsage {
                                    used_tokens: agent.estimated_context_tokens(),
                                }).await;
                                let _ = tx2.send(UiUpdate::Done { final_text }).await;
                            });
                        }
                    }
                    // History recall (Ctrl+Up / Ctrl+Down to avoid conflicting with pane scroll)
                    KeyCode::Up if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        if !app.input_history.is_empty() {
                            let idx = match app.history_index {
                                Some(i) if i > 0 => i - 1,
                                Some(i) => i,
                                None => app.input_history.len() - 1,
                            };
                            app.history_index = Some(idx);
                            app.input = app.input_history[idx].clone();
                            clamp_slash_selection(&app.slash_commands, &app.input, &mut app.slash_selected);
                            app.needs_redraw = true;
                        }
                    }
                    KeyCode::Down if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        if let Some(i) = app.history_index {
                            if i + 1 < app.input_history.len() {
                                let next = i + 1;
                                app.history_index = Some(next);
                                app.input = app.input_history[next].clone();
                            } else {
                                app.history_index = None;
                                app.input.clear();
                            }
                            clamp_slash_selection(&app.slash_commands, &app.input, &mut app.slash_selected);
                            app.needs_redraw = true;
                        }
                    }

                    KeyCode::Char(c) if !app.mode_menu_active && app.pending_approval.is_none() => {
                       app.input.push(c);
                        app.history_index = None;
                        clamp_slash_selection(&app.slash_commands, &app.input, &mut app.slash_selected);
                       app.needs_redraw = true;
                    }
                    KeyCode::Backspace if !app.mode_menu_active && app.pending_approval.is_none() => {
                       app.input.pop();
                        app.history_index = None;
                        clamp_slash_selection(&app.slash_commands, &app.input, &mut app.slash_selected);
                       app.needs_redraw = true;
                    }
                    KeyCode::PageUp if !app.mode_menu_active && key.modifiers.contains(KeyModifiers::SHIFT) => { app.right_follow_output = false; app.right_scroll = app.right_scroll.saturating_sub(12); }
                    KeyCode::PageUp if !app.mode_menu_active => { app.left_follow_output = false; app.left_scroll = app.left_scroll.saturating_sub(12); }
                    KeyCode::PageDown if !app.mode_menu_active && key.modifiers.contains(KeyModifiers::SHIFT) => { app.right_scroll = app.right_scroll.saturating_add(12); }
                    KeyCode::PageDown if !app.mode_menu_active => { app.left_scroll = app.left_scroll.saturating_add(12); }
                    KeyCode::Up if !app.mode_menu_active && key.modifiers.contains(KeyModifiers::SHIFT) => { app.right_follow_output = false; app.right_scroll = app.right_scroll.saturating_sub(1); }
                    KeyCode::Up if !app.mode_menu_active => { app.left_follow_output = false; app.left_scroll = app.left_scroll.saturating_sub(1); }
                    KeyCode::Down if !app.mode_menu_active && key.modifiers.contains(KeyModifiers::SHIFT) => { app.right_scroll = app.right_scroll.saturating_add(1); }
                    KeyCode::Down if !app.mode_menu_active => { app.left_scroll = app.left_scroll.saturating_add(1); }
                    _ => {}
                }
                } // if !app.settings_active
            }
            _ => {} // Timeout, non-key event, or channel closed
        }
    }
}

enum UiUpdate {
    Token(String),
    Thinking(String),
    ToolStart { name: String, args: String },
    ToolResult { name: String, summary: String },
    RoundLimitHit { continuation: u32, max_continuations: u32, exhausted: bool },
    Done { final_text: String },
    Error(String),
    ApprovalRequested,  // wake the UI so it can display the dialog from the approval channel
    ContextUsage { used_tokens: u32 }, // pushed from agent task after tool rounds
}


/// Truncate a string for display, respecting UTF-8 char boundaries.
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut end = max;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &s[..end])
    }
}
