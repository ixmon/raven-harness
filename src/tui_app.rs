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
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, LineGauge, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap},
    Terminal,
};
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::mpsc;

use crate::agent::Agent;
use crate::config::Config;
use crate::llm::{StreamChunk, ToolCall};

#[derive(Clone, Debug)]
struct SlashCommand {
    name: &'static str,
    desc: &'static str,
}

fn get_filtered_commands<'a>(commands: &'a [SlashCommand], input: &str) -> Vec<&'a SlashCommand> {
    if !input.starts_with('/') {
        return vec![];
    }
    let prefix = &input[1..].to_lowercase();
    commands
        .iter()
        .filter(|cmd| prefix.is_empty() || cmd.name.starts_with(prefix))
        .collect()
}

fn clamp_slash_selection(commands: &[SlashCommand], input: &str, selected: &mut usize) {
    let filtered = get_filtered_commands(commands, input);
    if !filtered.is_empty() {
        *selected = (*selected).min(filtered.len().saturating_sub(1));
    } else {
        *selected = 0;
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Pane {
    Left,
    Right,
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

    // Left pane: clean conversation (user prompts + final answers from turns)
    let mut left_committed: Vec<String> = vec![
        format!(
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
        ),
    ];

    // Live output from the *current* turn (flushed here on left)
    let mut current_response = String::new();

    // Right pane: thinking + tool call debug (separate from the main output)
    let mut trace_lines: Vec<String> = vec![];
    let mut current_thinking = String::new();  // live accumulation for thinking, flushed on boundaries

    let mut input = String::new();
    let mut left_scroll: u16 = 0;
    let mut right_scroll: u16 = 0;
    let mut left_follow_output = true;
    let mut right_follow_output = true;
    let mut is_processing = false;
    let mut focused_pane = Pane::Left; // Start with conversation pane focused
    let mut scroll_flash_timer: u8 = 0; // Flash effect timer for when arrow keys hit scroll limit
    let mut spinner_tick: usize = 0; // Animated throbber for processing state
    let mut tool_calls_this_turn: usize = 0; // Count tool calls for context gauge
    let mut turn_rounds: usize = 0; // Count inference rounds this turn
    let mut ctx_used_tokens: u32 = 0; // Updated via UiUpdate::ContextUsage from agent task

    // Execution approval state
    let mut pending_approval: Option<String> = None; // description of action needing approval
    let mut approval_responder: Option<tokio::sync::oneshot::Sender<bool>> = None;
    let mut needs_approval_redraw = false; // one-shot flag to force redraw when new approval arrives

    // current mode is read from agent/session when needed, default Babysitter

    let approval_modes: [&str; 4] = [
        "Babysitter - Always Ask",
        "Spring Break - Yolo for remainder of session",
        "Vegas - Yolo in sandbox",
        "Thunderdome - eternal Yolo, anytime, anywhere",
    ];
    let mut mode_menu_active = false;
    let mut selected_mode_idx: usize = 0;

    // Slash command menu state
    let slash_commands: Vec<SlashCommand> = vec![
        SlashCommand { name: "help",        desc: "Show available / commands" },
        SlashCommand { name: "clear",       desc: "Clear conversation history" },
        SlashCommand { name: "clear-trace", desc: "Clear only the trace pane" },
        SlashCommand { name: "reset",       desc: "Reset conversation (keeps goals/session)" },
        SlashCommand { name: "status",      desc: "Show current config and session info" },
        SlashCommand { name: "mode",        desc: "Change execution approval mode" },
        SlashCommand { name: "settings",    desc: "Manage inference endpoints" },
        SlashCommand { name: "quit",        desc: "Exit the TUI" },
    ];
    let mut slash_selected: usize = 0;

    // --- Mutable display state (updated on endpoint switch) ---
    let mut display_model = config.model.clone();
    let mut display_budget = config.context_budget.clone();

    // --- Settings modal state ---
    let mut settings_active = false;
    let mut settings_endpoints: Vec<crate::config::InferenceEndpoint> = vec![];
    let mut settings_selected: usize = 0;
    let mut active_endpoint_idx: usize = 0; // 0 = CLI default
    // Field editing: None = browsing list, Some(field_index) = editing that field
    // Fields: 0=label, 1=base_url, 2=model, 3=api_key
    #[allow(unused_assignments)]
    let mut settings_editing: Option<usize> = None;
    let mut settings_edit_buf = String::new();
    let mut settings_adding = false; // true when in "add new endpoint" flow
    let mut settings_add_step: usize = 0; // which field we're prompting for
    let mut settings_new_label = String::new();
    let mut settings_new_url = String::new();
    let mut settings_new_model = String::new();
    let mut settings_new_key = String::new();

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

    // These track layout info from the last draw so key handlers can reference them
    let mut last_left_line_count: u16 = 0;
    let mut last_right_line_count: u16 = 0;
    let mut last_left_area = ratatui::layout::Rect::default();
    let mut last_right_area = ratatui::layout::Rect::default();

    loop {
        // Advance spinner
        if is_processing {
            spinner_tick = spinner_tick.wrapping_add(1);
        }

        // Read current state from agent (try_lock to avoid blocking the draw).
        // ctx_used_tokens is NOT read here — it's pushed via UiUpdate::ContextUsage
        // from the agent task to avoid any lock contention during processing.
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
            ("…".to_string(), "…".into())
        };

        // Draw - split into status bar + content area + context gauge + input bar
        terminal.draw(|f| {
            let size = f.area();

            // Vertical layout: status bar (1) + content panes (fill) + context gauge (1) + input bar (3)
            let show_gauge = is_processing;
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
            let ctx = &display_budget;
            let status_spans = vec![
                Span::styled(" 🏛 ", Style::default().fg(Color::Rgb(0xc0, 0x80, 0xff))),
                Span::styled(&display_model, Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                Span::styled("  │  ", Style::default().fg(Color::DarkGray)),
                Span::styled("ctx:", Style::default().fg(Color::DarkGray)),
                Span::styled({
                    let used_k = ctx_used_tokens / 1000;
                    let max_k = ctx.context_tokens / 1000;
                    format!("{}k/{}k", used_k, max_k)
                }, {
                    let ratio = ctx_used_tokens as f64 / ctx.context_tokens.max(1) as f64;
                    if ratio < 0.5 {
                        Style::default().fg(Color::Rgb(0x80, 0xd0, 0x80)) // green
                    } else if ratio < 0.8 {
                        Style::default().fg(Color::Rgb(0xff, 0xc0, 0x40)) // amber
                    } else {
                        Style::default().fg(Color::Rgb(0xff, 0x60, 0x60)) // red
                    }
                }),
                Span::styled(
                    format!("({})", ctx.source),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled("  │  ", Style::default().fg(Color::DarkGray)),
                Span::styled("mode:", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    mode_label.split(" - ").next().unwrap_or("?"),
                    Style::default().fg(Color::Yellow),
                ),
                Span::styled("  │  ", Style::default().fg(Color::DarkGray)),
                Span::styled("goal:", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    truncate(&goal_text, 40),
                    Style::default().fg(Color::Rgb(0xa0, 0xd0, 0xff)),
                ),
            ];
            let status_line = Paragraph::new(Line::from(status_spans));
            f.render_widget(status_line, status_area);

            // ═══════════════════ CONTENT PANES ═══════════════════
            let panes = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(62), Constraint::Percentage(38)])
                .split(content_area);

            let left_area = panes[0];
            let right_area = panes[1];
            last_left_area = left_area;
            last_right_area = right_area;

            // ---- Left pane: styled conversation messages ----
            let mut left_text = Text::default();

            for (i, entry) in left_committed.iter().enumerate() {
                // Determine entry type by prefix for color coding
                let (prefix_style, body_style) = if entry.starts_with("You: ") {
                    (
                        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                        Style::default().fg(Color::Rgb(0xb0, 0xe0, 0xff)),
                    )
                } else if entry.starts_with("Agent: ") || entry.starts_with("Agent (partial): ") {
                    (
                        Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
                        Style::default().fg(Color::Rgb(0xd0, 0xf0, 0xd0)),
                    )
                } else if entry.contains("ERROR") || entry.starts_with("⚠") {
                    (
                        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                        Style::default().fg(Color::Rgb(0xff, 0xa0, 0xa0)),
                    )
                } else if entry.starts_with("✅") || entry.starts_with("⛔") || entry.starts_with("⏹") || entry.starts_with("🔒") {
                    (
                        Style::default().fg(Color::Yellow),
                        Style::default().fg(Color::Yellow),
                    )
                } else {
                    // System messages, welcome banner, etc.
                    (
                        Style::default().fg(Color::Rgb(0x88, 0x88, 0xaa)),
                        Style::default().fg(Color::Rgb(0x88, 0x88, 0xaa)),
                    )
                };

                let lines_iter: Vec<&str> = entry.lines().collect();
                for (li, line) in lines_iter.iter().enumerate() {
                    let style = if li == 0 { prefix_style } else { body_style };
                    left_text.lines.push(Line::from(Span::styled(line.to_string(), style)));
                }
                if i < left_committed.len() - 1 {
                    left_text.lines.push(Line::from(""));
                }
            }

            if !current_response.is_empty() {
                left_text.lines.push(Line::from(Span::styled(
                    "Agent (streaming):",
                    Style::default().fg(Color::Green).add_modifier(Modifier::BOLD | Modifier::ITALIC),
                )));
                for line in current_response.lines() {
                    left_text.lines.push(Line::from(Span::styled(
                        line.to_string(),
                        Style::default().fg(Color::Rgb(0xd0, 0xf0, 0xd0)),
                    )));
                }
            }

            let left_line_count = left_text.lines.len() as u16;
            last_left_line_count = left_line_count;
            let content_height = left_area.height.saturating_sub(2);
            if left_follow_output {
                left_scroll = left_line_count.saturating_sub(content_height);
            }
            left_scroll = left_scroll.min(left_line_count.saturating_sub(1));

            let focus_style = if focused_pane == Pane::Left {
                if scroll_flash_timer > 0 {
                    Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
                }
            } else {
                Style::default().fg(Color::Rgb(0x55, 0x55, 0x66))
            };

            // Rich block title with styled spans
            let left_title = Line::from(vec![
                Span::styled("  Conversation", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                Span::styled(
                    format!("  ({} msgs)", left_committed.len()),
                    Style::default().fg(Color::DarkGray),
                ),
            ]);

            let left_block = Paragraph::new(left_text)
                .block(
                    Block::default()
                        .title(left_title)
                        .borders(Borders::ALL)
                        .border_type(BorderType::Rounded)
                        .border_style(focus_style),
                )
                .wrap(Wrap { trim: false })
                .scroll((left_scroll, 0));

            f.render_widget(left_block, left_area);

            // Ratatui Scrollbar for left pane
            if left_line_count > content_height {
                let mut sb_state = ScrollbarState::new(left_line_count as usize)
                    .position(left_scroll as usize);
                let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
                    .begin_symbol(None)
                    .end_symbol(None)
                    .track_symbol(Some("│"))
                    .thumb_symbol("█")
                    .track_style(Style::default().fg(Color::Rgb(0x33, 0x33, 0x44)))
                    .thumb_style(Style::default().fg(Color::Rgb(0x88, 0x88, 0xbb)));
                f.render_stateful_widget(scrollbar, left_area, &mut sb_state);
            }

            // ---- Right pane: thinking + tool call trace ----
            let mut right_text = Text::default();

            for line in &trace_lines {
                // Color-code trace entries
                let style = if line.starts_with("🧠") {
                    Style::default().fg(Color::Rgb(0xd0, 0xa0, 0xff)).add_modifier(Modifier::ITALIC)
                } else if line.starts_with("🔧") {
                    Style::default().fg(Color::Rgb(0xff, 0xc0, 0x60))
                } else if line.starts_with("   ↳") {
                    Style::default().fg(Color::Rgb(0x80, 0xb0, 0x80))
                } else if line.starts_with("▶") || line.starts_with("⟳") {
                    Style::default().fg(Color::Cyan)
                } else if line.starts_with("⏸") || line.starts_with("⏹") {
                    Style::default().fg(Color::Yellow)
                } else if line.starts_with("⚠") {
                    Style::default().fg(Color::Red)
                } else if line.starts_with("🔒") {
                    Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Rgb(0x88, 0x88, 0x88))
                };
                right_text.lines.push(Line::from(Span::styled(line.clone(), style)));
            }

            if !current_thinking.is_empty() {
                right_text.lines.push(Line::from(Span::styled(
                    format!("🧠 {}", current_thinking.trim()),
                    Style::default().fg(Color::Rgb(0xd0, 0xa0, 0xff)).add_modifier(Modifier::ITALIC),
                )));
            }

            let right_line_count = right_text.lines.len() as u16;
            last_right_line_count = right_line_count;
            let content_height = right_area.height.saturating_sub(2);
            if right_follow_output {
                right_scroll = right_line_count.saturating_sub(content_height);
            }
            right_scroll = right_scroll.min(right_line_count.saturating_sub(1));

            let right_focus_style = if focused_pane == Pane::Right {
                if scroll_flash_timer > 0 {
                    Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
                }
            } else {
                Style::default().fg(Color::Rgb(0x55, 0x55, 0x66))
            };

            let right_title = Line::from(vec![
                Span::styled("  Trace", Style::default().fg(Color::Rgb(0xd0, 0xa0, 0xff)).add_modifier(Modifier::BOLD)),
                Span::styled("  (thinking + tools)", Style::default().fg(Color::DarkGray)),
            ]);

            let right_block = Paragraph::new(right_text)
                .block(
                    Block::default()
                        .title(right_title)
                        .borders(Borders::ALL)
                        .border_type(BorderType::Rounded)
                        .border_style(right_focus_style),
                )
                .wrap(Wrap { trim: false })
                .scroll((right_scroll, 0));

            f.render_widget(right_block, right_area);

            // Ratatui Scrollbar for right pane
            if right_line_count > content_height {
                let mut sb_state = ScrollbarState::new(right_line_count as usize)
                    .position(right_scroll as usize);
                let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
                    .begin_symbol(None)
                    .end_symbol(None)
                    .track_symbol(Some("│"))
                    .thumb_symbol("█")
                    .track_style(Style::default().fg(Color::Rgb(0x33, 0x33, 0x44)))
                    .thumb_style(Style::default().fg(Color::Rgb(0xd0, 0xa0, 0xff)));
                f.render_stateful_widget(scrollbar, right_area, &mut sb_state);
            }

            // ═══════════════════ CONTEXT GAUGE ═══════════════════
            if show_gauge {
                let max_rounds = config.max_rounds.min(12) as f64;
                let ratio = (turn_rounds as f64 / max_rounds).min(1.0);
                let gauge_label = format!(
                    " round {}/{} • {} tool calls",
                    turn_rounds,
                    max_rounds as u32,
                    tool_calls_this_turn,
                );
                let gauge_color = if ratio < 0.5 {
                    Color::Rgb(0x60, 0xd0, 0x80) // green
                } else if ratio < 0.8 {
                    Color::Rgb(0xff, 0xc0, 0x40) // amber
                } else {
                    Color::Rgb(0xff, 0x60, 0x60) // red
                };
                let gauge = LineGauge::default()
                    .ratio(ratio)
                    .label(Line::from(Span::styled(gauge_label, Style::default().fg(Color::White))))
                    .filled_style(Style::default().fg(gauge_color))
                    .unfilled_style(Style::default().fg(Color::Rgb(0x33, 0x33, 0x44)))
                    .line_set(ratatui::symbols::line::THICK);
                f.render_widget(gauge, gauge_area);
            }

            // ═══════════════════ INPUT BAR ═══════════════════
            let spinner_frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
            let input_title = if input.starts_with('/') {
                Line::from(vec![
                    Span::styled(" / ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                    Span::styled("Commands", Style::default().fg(Color::Gray)),
                    Span::styled("  ↑↓ select • Tab complete • Enter run • Esc clear ", Style::default().fg(Color::DarkGray)),
                ])
            } else if is_processing {
                let frame = spinner_frames[spinner_tick % spinner_frames.len()];
                Line::from(vec![
                    Span::styled(format!(" {} ", frame), Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                    Span::styled("Processing", Style::default().fg(Color::Cyan)),
                    Span::styled("  Esc", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
                    Span::styled(" to STOP  ", Style::default().fg(Color::DarkGray)),
                    Span::styled("Ctrl-C", Style::default().fg(Color::Red)),
                    Span::styled(" quit ", Style::default().fg(Color::DarkGray)),
                ])
            } else {
                Line::from(vec![
                    Span::styled(" > ", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
                    Span::styled("Input", Style::default().fg(Color::Gray)),
                    Span::styled("  Enter send • Ctrl-C quit • ", Style::default().fg(Color::DarkGray)),
                    Span::styled("/", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                    Span::styled(" commands ", Style::default().fg(Color::DarkGray)),
                ])
            };
            let input_para = Paragraph::new(input.as_str())
                .style(Style::default().fg(Color::White))
                .block(
                    Block::default()
                        .title(input_title)
                        .borders(Borders::ALL)
                        .border_type(BorderType::Rounded)
                        .border_style(Style::default().fg(Color::Rgb(0x55, 0x55, 0x66)))
                );
            f.render_widget(input_para, input_area);

            // Approval dialog popup above the input bar
            if let Some(ref desc) = pending_approval {
                let pw = 60u16;
                let ph = 9u16;
                let px = 2;
                let py = input_area.y.saturating_sub(ph + 1);
                let pa = ratatui::layout::Rect::new(px, py, pw, ph);
                let safe_desc = if desc.len() > 220 {
                    format!("{}…", &desc[..220])
                } else {
                    desc.clone()
                };
                let popup_text = Text::from(vec![
                    Line::from(Span::styled(
                        "Sandbox approval needed",
                        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                    )),
                    Line::from(""),
                    Line::from(Span::styled(safe_desc, Style::default().fg(Color::White))),
                    Line::from(""),
                    Line::from(vec![
                        Span::styled("[Y]", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
                        Span::styled("es  ", Style::default().fg(Color::Gray)),
                        Span::styled("[N]", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
                        Span::styled("o  ", Style::default().fg(Color::Gray)),
                        Span::styled("(Esc)", Style::default().fg(Color::DarkGray)),
                    ]),
                ]);
                let popup = Paragraph::new(popup_text)
                    .wrap(Wrap { trim: true })
                    .block(
                        Block::default()
                            .title(Span::styled(" Action Approval ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)))
                            .borders(Borders::ALL)
                            .border_type(BorderType::Double)
                            .border_style(Style::default().fg(Color::Yellow))
                    );
                f.render_widget(popup, pa);
            }

            // Slash command popup menu (rendered above the input bar)
            if input.starts_with('/') {
                let filtered = get_filtered_commands(&slash_commands, &input);
                if !filtered.is_empty() {
                    let max_visible = 7usize;
                    let visible = filtered.len().min(max_visible);
                    let extra = if filtered.len() > max_visible { 1 } else { 0 };
                    let menu_h = (visible as u16) + 1 /* header */ + extra + 2 /* borders */;

                    let menu_area = ratatui::layout::Rect {
                        x: input_area.x,
                        y: input_area.y.saturating_sub(menu_h),
                        width: input_area.width.min(58),
                        height: menu_h,
                    };

                    let sel = slash_selected.min(filtered.len().saturating_sub(1));

                    let mut menu_text = Text::default();
                    menu_text.lines.push(Line::from(Span::styled(
                        "  / Commands",
                        Style::default().fg(Color::Gray).add_modifier(Modifier::BOLD),
                    )));

                    for (i, cmd) in filtered.iter().enumerate().take(max_visible) {
                        let is_selected = i == sel;
                        let marker = if is_selected { "▶ " } else { "  " };
                        let name_style = if is_selected {
                            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(Color::White)
                        };
                        let desc_style = Style::default().fg(Color::DarkGray);

                        let mut spans = vec![
                            Span::styled(marker, if is_selected { Style::default().fg(Color::Cyan) } else { Style::default().fg(Color::Gray) }),
                            Span::styled(format!("/{}", cmd.name), name_style),
                        ];
                        if !cmd.desc.is_empty() {
                            spans.push(Span::styled(format!("  — {}", cmd.desc), desc_style));
                        }
                        menu_text.lines.push(Line::from(spans));
                    }

                    if filtered.len() > max_visible {
                        menu_text.lines.push(Line::from(Span::styled("   …", Style::default().fg(Color::DarkGray))));
                    }

                    let menu_block = Paragraph::new(menu_text)
                        .block(
                            Block::default()
                                .borders(Borders::ALL)
                                .border_type(BorderType::Rounded)
                                .border_style(Style::default().fg(Color::Rgb(0x55, 0x55, 0x55)))
                        );

                    f.render_widget(menu_block, menu_area);
                }
            }

            // Mode selection menu popup (for /mode)
            if mode_menu_active {
                let desired_h = 1u16 /* header */ + approval_modes.len() as u16 + 2 /* borders */;
                let menu_h = if input_area.y >= desired_h { desired_h } else { input_area.y.max(4) };
                let menu_area = ratatui::layout::Rect {
                    x: input_area.x,
                    y: input_area.y.saturating_sub(menu_h),
                    width: input_area.width.min(62),
                    height: menu_h,
                };

                let mut menu_text = Text::default();
                menu_text.lines.push(Line::from(Span::styled(
                    "  Execution Mode",
                    Style::default().fg(Color::Gray).add_modifier(Modifier::BOLD),
                )));

                for (i, m) in approval_modes.iter().enumerate() {
                    let is_sel = i == selected_mode_idx;
                    let marker = if is_sel { "▶ " } else { "  " };
                    let style = if is_sel {
                        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::White)
                    };
                    menu_text.lines.push(Line::from(Span::styled(format!("{}{}", marker, m), style)));
                }

                let menu_block = Paragraph::new(menu_text)
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .border_type(BorderType::Rounded)
                            .border_style(Style::default().fg(Color::Rgb(0x55, 0x55, 0x55)))
                    );
                f.render_widget(menu_block, menu_area);
            }

            // Settings modal (full overlay)
            if settings_active {
                let modal_w = 64u16.min(size.width.saturating_sub(4));
                let modal_h = 22u16.min(size.height.saturating_sub(4));
                let modal_x = (size.width.saturating_sub(modal_w)) / 2;
                let modal_y = (size.height.saturating_sub(modal_h)) / 2;
                let modal_area = ratatui::layout::Rect::new(modal_x, modal_y, modal_w, modal_h);

                let mut modal_lines = Text::default();

                if settings_adding {
                    // Add-new-endpoint wizard
                    modal_lines.lines.push(Line::from(Span::styled(
                        "  Add New Endpoint",
                        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                    )));
                    modal_lines.lines.push(Line::from(""));

                    let fields = ["Label", "Base URL", "Model", "API Key (optional)"];
                    let values = [&settings_new_label, &settings_new_url, &settings_new_model, &settings_new_key];
                    for (i, (field, val)) in fields.iter().zip(values.iter()).enumerate() {
                        let is_current = i == settings_add_step;
                        let marker = if i < settings_add_step {
                            "✓"
                        } else if is_current {
                            "▶"
                        } else {
                            " "
                        };
                        let label_style = if is_current {
                            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
                        } else if i < settings_add_step {
                            Style::default().fg(Color::Green)
                        } else {
                            Style::default().fg(Color::DarkGray)
                        };
                        let display_val = if i == 3 && !val.is_empty() {
                            "*".repeat(val.len().min(20))
                        } else {
                            val.to_string()
                        };

                        if is_current {
                            modal_lines.lines.push(Line::from(vec![
                                Span::styled(format!("  {} ", marker), label_style),
                                Span::styled(format!("{}: ", field), label_style),
                                Span::styled(&settings_edit_buf, Style::default().fg(Color::White)),
                                Span::styled("_", Style::default().fg(Color::Cyan).add_modifier(Modifier::SLOW_BLINK)),
                            ]));
                        } else {
                            modal_lines.lines.push(Line::from(vec![
                                Span::styled(format!("  {} ", marker), label_style),
                                Span::styled(format!("{}: ", field), label_style),
                                Span::styled(display_val, Style::default().fg(Color::Gray)),
                            ]));
                        }
                    }
                    modal_lines.lines.push(Line::from(""));
                    modal_lines.lines.push(Line::from(Span::styled(
                        "  Enter to confirm field • Esc to cancel",
                        Style::default().fg(Color::DarkGray),
                    )));
                } else {
                    // Endpoint list view
                    modal_lines.lines.push(Line::from(Span::styled(
                        "  Inference Endpoints",
                        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                    )));
                    modal_lines.lines.push(Line::from(""));

                    for (i, ep) in settings_endpoints.iter().enumerate() {
                        let is_sel = i == settings_selected;
                        let is_active = i == active_endpoint_idx;
                        let marker = if is_active { "●" } else { "○" };
                        let name_style = if is_sel {
                            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(Color::White)
                        };
                        let sel_indicator = if is_sel { "▶ " } else { "  " };

                        modal_lines.lines.push(Line::from(vec![
                            Span::styled(sel_indicator, if is_sel { Style::default().fg(Color::Cyan) } else { Style::default().fg(Color::Gray) }),
                            Span::styled(format!("{} ", marker), if is_active { Style::default().fg(Color::Green) } else { Style::default().fg(Color::DarkGray) }),
                            Span::styled(&ep.label, name_style),
                            if is_active {
                                Span::styled("  [active]", Style::default().fg(Color::Green))
                            } else {
                                Span::styled("", Style::default())
                            },
                        ]));

                        // Show URL + model on second line
                        modal_lines.lines.push(Line::from(vec![
                            Span::styled("      ", Style::default()),
                            Span::styled(&ep.base_url, Style::default().fg(Color::DarkGray)),
                        ]));
                        let key_indicator = if ep.api_key.is_some() { "  🔑" } else { "" };
                        modal_lines.lines.push(Line::from(vec![
                            Span::styled("      model: ", Style::default().fg(Color::DarkGray)),
                            Span::styled(&ep.model, Style::default().fg(Color::Gray)),
                            Span::styled(key_indicator, Style::default().fg(Color::Yellow)),
                        ]));

                        if i < settings_endpoints.len() - 1 {
                            modal_lines.lines.push(Line::from(""));
                        }
                    }

                    modal_lines.lines.push(Line::from(""));
                    modal_lines.lines.push(Line::from(vec![
                        Span::styled("  ↑↓ ", Style::default().fg(Color::DarkGray)),
                        Span::styled("navigate", Style::default().fg(Color::Gray)),
                        Span::styled("  Enter ", Style::default().fg(Color::DarkGray)),
                        Span::styled("switch", Style::default().fg(Color::Gray)),
                        Span::styled("  A ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                        Span::styled("add", Style::default().fg(Color::Gray)),
                    ]));
                    modal_lines.lines.push(Line::from(vec![
                        Span::styled("  D ", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
                        Span::styled("delete", Style::default().fg(Color::Gray)),
                        Span::styled("  Esc ", Style::default().fg(Color::DarkGray)),
                        Span::styled("close", Style::default().fg(Color::Gray)),
                    ]));
                }

                let modal_block = Paragraph::new(modal_lines)
                    .wrap(Wrap { trim: false })
                    .block(
                        Block::default()
                            .title(Span::styled(" Settings ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)))
                            .borders(Borders::ALL)
                            .border_type(BorderType::Double)
                            .border_style(Style::default().fg(Color::Cyan))
                    );
                // Clear the area behind the modal first to prevent bleed-through
                f.render_widget(ratatui::widgets::Clear, modal_area);
                f.render_widget(modal_block, modal_area);
            }

            // Cursor in input
            f.set_cursor_position((input_area.x + 1 + input.len() as u16, input_area.y + 1));
            
            // Decrement scroll flash timer
            if scroll_flash_timer > 0 {
                scroll_flash_timer = scroll_flash_timer.saturating_sub(1);
            }
        })?;

        // Poll for new approval requests from the agent background task
        while let Ok((desc, tx)) = approval_req_rx.try_recv() {
            pending_approval = Some(desc.clone());
            approval_responder = Some(tx);
            needs_approval_redraw = true; // force a redraw so popup shows
            input.clear(); // don't leave garbage in the input field
            trace_lines.push(format!("🔒 Approval requested for: {}", desc));
            left_committed.push(format!("🔒 Approval requested for: {}", desc));
            left_follow_output = true;
            left_scroll = 10_000;
            right_follow_output = true;
            right_scroll = 10_000;
        }
        // If we just picked up a NEW approval this iteration, force a redraw
        // so the popup is visible before we block on select! waiting for input.
        // The `needs_redraw` flag prevents a hot loop (only continue once).
        if needs_approval_redraw {
            needs_approval_redraw = false;
            continue; // → top of loop → draw (now shows popup) → select! for Y/N
        }

        // Handle input + agent updates (non-blocking)
        if is_processing {
            // While processing we mostly listen for agent updates and a few keys
            tokio::select! {
                Some(update) = rx.recv() => {
                    // Always poll for approval requests on any agent update to ensure dialog shows promptly
                    while let Ok((desc, tx)) = approval_req_rx.try_recv() {
                        pending_approval = Some(desc.clone());
                        approval_responder = Some(tx);
                        input.clear();
                        trace_lines.push(format!("🔒 Approval requested for: {}", desc));
                        left_committed.push(format!("🔒 Approval requested for: {}", desc));
                        left_follow_output = true;
                        left_scroll = 10_000;
                        right_follow_output = true;
                        right_scroll = 10_000;
                    }
                    match update {
                        UiUpdate::Token(t) => {
                            // Regular content tokens → live on the LEFT pane (current turn output)
                            current_response.push_str(&t);
                            left_follow_output = true;
                            left_scroll = 10_000; // auto-scroll to bottom while streaming output
                        }
                        UiUpdate::Thinking(t) => {
                            // Accumulate small thinking chunks (models often send 1-3 tokens at a time)
                            // and only commit to trace_lines on reasonable boundaries.
                            current_thinking.push_str(&t);
                            right_follow_output = true;
                            right_scroll = 10_000; // auto-scroll trace pane on new thinking

                            // Flush heuristic: paragraph break, sentence terminator + space, or size limit.
                            // This turns "one word per line" into proper sentences/paragraphs in the trace.
                            let should_flush =
                                current_thinking.contains("\n\n") ||
                                current_thinking.ends_with(". ") ||
                                current_thinking.ends_with("! ") ||
                                current_thinking.ends_with("? ") ||
                                current_thinking.len() > 160;

                            if should_flush {
                                let block = current_thinking.trim().to_string();
                                if !block.is_empty() {
                                    trace_lines.push(format!("🧠 {}", block));
                                    right_follow_output = true;
                                    right_scroll = 10_000;
                                }
                                current_thinking.clear();
                            }
                        }
                        UiUpdate::ToolStart { name, args } => {
                            // Tool activity → RIGHT pane (debug)
                            trace_lines.push(format!("🔧 {}({})", name, truncate(&args, 90)));
                            tool_calls_this_turn += 1;
                            right_follow_output = true;
                            right_scroll = 10_000;
                        }
                        UiUpdate::ToolResult { name, summary } => {
                            trace_lines.push(format!("   ↳ {} → {}", name, truncate(&summary, 120)));
                            right_follow_output = true;
                            right_scroll = 10_000;
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
                            trace_lines.push(msg);
                            turn_rounds += 1;
                            right_follow_output = true;
                            right_scroll = 10_000;
                        }
                        UiUpdate::Done { final_text } => {
                            // Turn complete. Flush the output.
                            // Use the Done's final_text as a robust fallback in case no individual
                            // Token updates were emitted by the stream (some llama.cpp configurations
                            // deliver the full response only in the final payload).
                            if current_response.trim().is_empty() && !final_text.trim().is_empty() {
                                current_response = final_text;
                            }
                            if !current_response.trim().is_empty() {
                                left_committed.push(format!("Agent: {}", current_response.trim()));
                                left_follow_output = true;
                                left_scroll = 10_000; // auto-scroll to bottom after agent response
                                current_response.clear();
                            }
                            // Flush any remaining live thinking
                            if !current_thinking.trim().is_empty() {
                                trace_lines.push(format!("🧠 {}", current_thinking.trim()));
                                current_thinking.clear();
                            }
                            if !trace_lines.is_empty() {
                                right_follow_output = true;
                                right_scroll = 10_000;
                            }
                            is_processing = false;
                        }
                        UiUpdate::Error(e) => {
                            let msg = format!("⚠ ERROR: {}", e);
                            trace_lines.push(msg.clone());
                            right_follow_output = true;
                            right_scroll = 10_000;
                            // Flush any pending thinking on error
                            if !current_thinking.trim().is_empty() {
                                trace_lines.push(format!("🧠 {}", current_thinking.trim()));
                                current_thinking.clear();
                            }
                            // Make errors visible in the main left pane too
                            left_committed.push(msg);
                            if !current_response.trim().is_empty() {
                                left_committed.push(format!("Agent (partial): {}", current_response.trim()));
                                current_response.clear();
                            }
                            is_processing = false;
                        }
                        UiUpdate::ApprovalRequested => {
                            // Poll the approval channel here to set pending immediately
                            while let Ok((desc, tx)) = approval_req_rx.try_recv() {
                                pending_approval = Some(desc);
                                approval_responder = Some(tx);
                                needs_approval_redraw = true;
                                input.clear();
                            }
                            // Force a redraw so the dialog appears immediately
                            left_follow_output = true;
                            right_follow_output = true;
                        }
                        UiUpdate::ContextUsage { used_tokens } => {
                            ctx_used_tokens = used_tokens;
                        }
                    }
                }

                // Allow the user to scroll AND type-ahead while processing
                Some(ev) = input_rx.recv() => {
                    if let Event::Key(key) = ev {
                        // Highest priority: approval dialog
                        if pending_approval.is_some() {
                            match key.code {
                                KeyCode::Char('y') | KeyCode::Char('Y') => {
                                    if let Some(tx) = approval_responder.take() {
                                        let _ = tx.send(true);
                                    }
                                    pending_approval = None;
                                    left_committed.push("✅ Action approved".to_string());
                                    left_follow_output = true;
                                    left_scroll = 10_000;
                                }
                                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                                    if let Some(tx) = approval_responder.take() {
                                        let _ = tx.send(false);
                                    }
                                    pending_approval = None;
                                    left_committed.push("⛔ Action denied".to_string());
                                    left_follow_output = true;
                                    left_scroll = 10_000;
                                }
                                _ => {}
                            }
                            continue;
                        }

                        // Handle settings modal if active
                        if settings_active {
                            if settings_adding {
                                // Add-new-endpoint wizard: typing into fields
                                match key.code {
                                    KeyCode::Char(c) => { settings_edit_buf.push(c); }
                                    KeyCode::Backspace => { settings_edit_buf.pop(); }
                                    KeyCode::Enter => {
                                        // Save current field and advance
                                        match settings_add_step {
                                            0 => {
                                                settings_new_label = settings_edit_buf.clone();
                                                settings_edit_buf = "https://openrouter.ai/api/v1".to_string();
                                                settings_add_step = 1;
                                            }
                                            1 => {
                                                settings_new_url = settings_edit_buf.clone();
                                                settings_edit_buf.clear();
                                                settings_add_step = 2;
                                            }
                                            2 => {
                                                settings_new_model = settings_edit_buf.clone();
                                                settings_edit_buf.clear();
                                                settings_add_step = 3;
                                            }
                                            3 => {
                                                settings_new_key = settings_edit_buf.clone();
                                                // All fields collected — save
                                                let api_key_opt = if settings_new_key.is_empty() {
                                                    None
                                                } else {
                                                    // Ensure vault is initialized
                                                    if !keystore.is_unlocked() {
                                                        // Need a vault password — for now use a simple prompt via trace
                                                        // In practice we'd prompt in the modal, but this is simpler
                                                        left_committed.push("Setting vault password (first API key). Using 'raven' as default — change in endpoints.json.".to_string());
                                                        let _ = keystore.init_password("raven");
                                                    }
                                                    Some(settings_new_key.as_str())
                                                };
                                                match keystore.add_endpoint(
                                                    &settings_new_label,
                                                    &settings_new_url,
                                                    &settings_new_model,
                                                    api_key_opt,
                                                ) {
                                                    Ok(()) => {
                                                        left_committed.push(format!("Added endpoint: {}", settings_new_label));
                                                        left_follow_output = true;
                                                        left_scroll = 10_000;
                                                    }
                                                    Err(e) => {
                                                        left_committed.push(format!("⚠ Failed to save endpoint: {}", e));
                                                        left_follow_output = true;
                                                        left_scroll = 10_000;
                                                    }
                                                }
                                                // Rebuild list
                                                settings_endpoints = vec![crate::config::InferenceEndpoint::from_config(&config)];
                                                if let Ok(eps) = keystore.decrypt_all_endpoints() {
                                                    settings_endpoints.extend(eps);
                                                }
                                                settings_adding = false;
                                                settings_add_step = 0;
                                                settings_edit_buf.clear();
                                                settings_new_label.clear();
                                                settings_new_url.clear();
                                                settings_new_model.clear();
                                                settings_new_key.clear();
                                            }
                                            _ => {}
                                        }
                                    }
                                    KeyCode::Esc => {
                                        settings_adding = false;
                                        settings_add_step = 0;
                                        settings_edit_buf.clear();
                                        settings_new_label.clear();
                                        settings_new_url.clear();
                                        settings_new_model.clear();
                                        settings_new_key.clear();
                                    }
                                    _ => {}
                                }
                            } else {
                                // Endpoint list navigation
                                match key.code {
                                    KeyCode::Up | KeyCode::Char('k') => {
                                        if settings_selected > 0 { settings_selected -= 1; }
                                    }
                                    KeyCode::Down | KeyCode::Char('j') => {
                                        if settings_selected < settings_endpoints.len().saturating_sub(1) {
                                            settings_selected += 1;
                                        }
                                    }
                                    KeyCode::Enter => {
                                        // Switch to selected endpoint
                                        if settings_selected != active_endpoint_idx {
                                            let ep = &settings_endpoints[settings_selected];
                                            trace_lines.push(format!("⟳ Switching to: {} ({})", ep.label, ep.base_url));
                                            right_follow_output = true;
                                            right_scroll = 10_000;

                                            // Probe context (synchronous-ish — quick timeout)
                                            let probe_result = crate::llm::probe_context_size(
                                                &ep.base_url,
                                                &ep.model,
                                                ep.api_key.as_deref(),
                                            ).await;

                                            let budget = match probe_result {
                                                Some(n_ctx) => {
                                                    trace_lines.push(format!("   ↳ context: {} tokens (probed)", n_ctx));
                                                    crate::config::ContextBudget::from_context_tokens(n_ctx, config.max_rounds)
                                                }
                                                None => {
                                                    trace_lines.push("   ↳ context probe failed, using default 8192".to_string());
                                                    crate::config::ContextBudget::default_fallback()
                                                }
                                            };

                                            if let Ok(mut ag) = agent.try_lock() {
                                                ag.switch_endpoint(ep, budget.clone());
                                            }

                                            // Update display state so status bar reflects the switch
                                            display_model = ep.model.clone();
                                            display_budget = budget;

                                            active_endpoint_idx = settings_selected;
                                            left_committed.push(format!("Switched to: {} ({})", ep.label, ep.model));
                                            left_follow_output = true;
                                            left_scroll = 10_000;
                                        }
                                        settings_active = false;
                                    }
                                    KeyCode::Char('a') | KeyCode::Char('A') => {
                                        // Start add-new-endpoint wizard
                                        settings_adding = true;
                                        settings_add_step = 0;
                                        settings_edit_buf.clear();
                                        settings_new_label.clear();
                                        settings_new_url.clear();
                                        settings_new_model.clear();
                                        settings_new_key.clear();
                                    }
                                    KeyCode::Char('d') | KeyCode::Char('D') => {
                                        // Delete selected (but not the CLI default or active)
                                        if settings_selected > 0 && settings_selected != active_endpoint_idx {
                                            let keystore_idx = settings_selected - 1; // offset for CLI entry
                                            let _ = keystore.remove_endpoint(keystore_idx);
                                            // Rebuild list
                                            settings_endpoints = vec![crate::config::InferenceEndpoint::from_config(&config)];
                                            if let Ok(eps) = keystore.decrypt_all_endpoints() {
                                                settings_endpoints.extend(eps);
                                            }
                                            if settings_selected >= settings_endpoints.len() {
                                                settings_selected = settings_endpoints.len().saturating_sub(1);
                                            }
                                            // Adjust active index if needed
                                            if active_endpoint_idx > settings_selected {
                                                active_endpoint_idx = active_endpoint_idx.saturating_sub(1);
                                            }
                                            left_committed.push("Endpoint deleted.".to_string());
                                            left_follow_output = true;
                                            left_scroll = 10_000;
                                        }
                                    }
                                    KeyCode::Esc => {
                                        settings_active = false;
                                    }
                                    _ => {}
                                }
                            }
                            continue;
                        }

                        // Handle /mode selection menu first if active
                        if mode_menu_active {
                            match key.code {
                                KeyCode::Up | KeyCode::Char('k') => { if selected_mode_idx > 0 { selected_mode_idx -= 1; } }
                                KeyCode::Down | KeyCode::Char('j') => { if selected_mode_idx < 3 { selected_mode_idx += 1; } }
                                KeyCode::Enter => {
                                    let chosen = approval_modes[selected_mode_idx];
                                    left_committed.push(format!("Execution mode set to: {}", chosen));
                                    left_follow_output = true;
                                    left_scroll = 10_000;

                                    match selected_mode_idx {
                                        0 => { if let Ok(mut ag) = agent.try_lock() { ag.set_exec_approval_mode(crate::session::ExecApprovalMode::Babysitter); if let Some(s) = &mut ag.session { let _ = s.save_meta(); } } }
                                        1 => { if let Ok(mut ag) = agent.try_lock() { ag.set_exec_approval_mode(crate::session::ExecApprovalMode::SpringBreak); if let Some(s) = &mut ag.session { let _ = s.save_meta(); } } }
                                        2 => { if let Ok(mut ag) = agent.try_lock() { ag.set_exec_approval_mode(crate::session::ExecApprovalMode::Vegas); if let Some(s) = &mut ag.session { let _ = s.save_meta(); } } }
                                        3 => { if let Ok(mut ag) = agent.try_lock() { ag.set_exec_approval_mode(crate::session::ExecApprovalMode::Thunderdome); if let Some(s) = &mut ag.session { let _ = s.save_meta(); } } }
                                        _ => {}
                                    }
                                    mode_menu_active = false;
                                    input.clear();
                                    selected_mode_idx = 0;
                                }
                                KeyCode::Esc => {
                                    mode_menu_active = false;
                                    input.clear();
                                    selected_mode_idx = 0;
                                }
                                _ => {}
                            }
                            continue;
                        }

                        match key.code {
                            KeyCode::Tab => {
                                focused_pane = match focused_pane {
                                    Pane::Left => Pane::Right,
                                    Pane::Right => Pane::Left,
                                };
                                continue; // Force redraw to show focus change
                            }
                            KeyCode::Esc => {
                                // STOP: signal the agent task to halt at the next clean point
                                stop_signal.store(true, Ordering::SeqCst);
                                // If there's a pending approval, deny it automatically
                                if let Some(tx) = approval_responder.take() {
                                    let _ = tx.send(false);
                                }
                                pending_approval = None;
                                trace_lines.push("⏹ Stop requested by user (Esc)".to_string());
                                left_committed.push("⏹ Stopping agent...".to_string());
                                left_follow_output = true;
                                left_scroll = 10_000;
                                right_follow_output = true;
                                right_scroll = 10_000;
                            }

                            KeyCode::PageUp if focused_pane == Pane::Right => { right_follow_output = false; let old_scroll = right_scroll; right_scroll = right_scroll.saturating_sub(8); if old_scroll == right_scroll && old_scroll == 0 { scroll_flash_timer = 10; } }
                            KeyCode::PageUp if focused_pane == Pane::Left => { left_follow_output = false; let old_scroll = left_scroll; left_scroll = left_scroll.saturating_sub(8); if old_scroll == left_scroll && old_scroll == 0 { scroll_flash_timer = 10; } }
                            KeyCode::PageDown if focused_pane == Pane::Right => { let old_scroll = right_scroll; right_scroll = right_scroll.saturating_add(8); let right_max = last_right_line_count.saturating_sub(last_right_area.height.saturating_sub(2)); if old_scroll == right_scroll && old_scroll >= right_max { scroll_flash_timer = 10; } }
                            KeyCode::PageDown if focused_pane == Pane::Left => { let old_scroll = left_scroll; left_scroll = left_scroll.saturating_add(8); let left_max = last_left_line_count.saturating_sub(last_left_area.height.saturating_sub(2)); if old_scroll == left_scroll && old_scroll >= left_max { scroll_flash_timer = 10; } }
                            KeyCode::Up if focused_pane == Pane::Right => { right_follow_output = false; let old_scroll = right_scroll; right_scroll = right_scroll.saturating_sub(1); if old_scroll == right_scroll && old_scroll == 0 { scroll_flash_timer = 10; } }
                            KeyCode::Up if focused_pane == Pane::Left => { left_follow_output = false; let old_scroll = left_scroll; left_scroll = left_scroll.saturating_sub(1); if old_scroll == left_scroll && old_scroll == 0 { scroll_flash_timer = 10; } }
                            KeyCode::Down if focused_pane == Pane::Right => { let old_scroll = right_scroll; right_scroll = right_scroll.saturating_add(1); let right_max = last_right_line_count.saturating_sub(last_right_area.height.saturating_sub(2)); if old_scroll == right_scroll && old_scroll >= right_max { scroll_flash_timer = 10; } }
                            KeyCode::Down if focused_pane == Pane::Left => { let old_scroll = left_scroll; left_scroll = left_scroll.saturating_add(1); let left_max = last_left_line_count.saturating_sub(last_left_area.height.saturating_sub(2)); if old_scroll == left_scroll && old_scroll >= left_max { scroll_flash_timer = 10; } }
                            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                return Ok(());
                            }

                            // Allow typing ahead while the model is working
                            KeyCode::Char(c) => { input.push(c); clamp_slash_selection(&slash_commands, &input, &mut slash_selected); }
                            KeyCode::Backspace => { input.pop(); clamp_slash_selection(&slash_commands, &input, &mut slash_selected); }
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
                if pending_approval.is_some() {
                    match key.code {
                        KeyCode::Char('y') | KeyCode::Char('Y') => {
                            if let Some(tx) = approval_responder.take() {
                                let _ = tx.send(true);
                            }
                            pending_approval = None;
                            left_committed.push("✅ Action approved".to_string());
                            left_follow_output = true;
                            left_scroll = 10_000;
                        }
                        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                            if let Some(tx) = approval_responder.take() {
                                let _ = tx.send(false);
                            }
                            pending_approval = None;
                            left_committed.push("⛔ Action denied".to_string());
                            left_follow_output = true;
                            left_scroll = 10_000;
                        }
                        _ => {}
                    }
                    // fallthrough to match below (input guards prevent typing; Enter dispatch will see cleared input)
                } else if mode_menu_active {
                    match key.code {
                        KeyCode::Up | KeyCode::Char('k') => { if selected_mode_idx > 0 { selected_mode_idx -= 1; } }
                        KeyCode::Down | KeyCode::Char('j') => { if selected_mode_idx < 3 { selected_mode_idx += 1; } }
                        KeyCode::Enter => {
                            let chosen = approval_modes[selected_mode_idx];
                            left_committed.push(format!("Execution mode set to: {}", chosen));
                            left_follow_output = true;
                            left_scroll = 10_000;

                            match selected_mode_idx {
                                0 => { if let Ok(mut ag) = agent.try_lock() { ag.set_exec_approval_mode(crate::session::ExecApprovalMode::Babysitter); if let Some(s) = &mut ag.session { let _ = s.save_meta(); } } }
                                1 => { if let Ok(mut ag) = agent.try_lock() { ag.set_exec_approval_mode(crate::session::ExecApprovalMode::SpringBreak); if let Some(s) = &mut ag.session { let _ = s.save_meta(); } } }
                                2 => { if let Ok(mut ag) = agent.try_lock() { ag.set_exec_approval_mode(crate::session::ExecApprovalMode::Vegas); if let Some(s) = &mut ag.session { let _ = s.save_meta(); } } }
                                3 => { if let Ok(mut ag) = agent.try_lock() { ag.set_exec_approval_mode(crate::session::ExecApprovalMode::Thunderdome); if let Some(s) = &mut ag.session { let _ = s.save_meta(); } } }
                                _ => {}
                            }
                            mode_menu_active = false;
                            input.clear();
                            selected_mode_idx = 0;
                        }
                        KeyCode::Esc => {
                            mode_menu_active = false;
                            input.clear();
                            selected_mode_idx = 0;
                        }
                        _ => {}
                    }
                    // fallthrough; big match below has guards so chars etc don't mutate input while menu is open
                } else if settings_active {
                    if settings_adding {
                        // Add-new-endpoint wizard: typing into fields
                        match key.code {
                            KeyCode::Char(c) => { settings_edit_buf.push(c); }
                            KeyCode::Backspace => { settings_edit_buf.pop(); }
                            KeyCode::Enter => {
                                match settings_add_step {
                                    0 => {
                                        settings_new_label = settings_edit_buf.clone();
                                        settings_edit_buf = "https://openrouter.ai/api/v1".to_string();
                                        settings_add_step = 1;
                                    }
                                    1 => {
                                        settings_new_url = settings_edit_buf.clone();
                                        settings_edit_buf.clear();
                                        settings_add_step = 2;
                                    }
                                    2 => {
                                        settings_new_model = settings_edit_buf.clone();
                                        settings_edit_buf.clear();
                                        settings_add_step = 3;
                                    }
                                    3 => {
                                        settings_new_key = settings_edit_buf.clone();
                                        let api_key_opt = if settings_new_key.is_empty() {
                                            None
                                        } else {
                                            if !keystore.is_unlocked() {
                                                left_committed.push("Setting vault password (first API key). Using 'raven' as default.".to_string());
                                                let _ = keystore.init_password("raven");
                                            }
                                            Some(settings_new_key.as_str())
                                        };
                                        match keystore.add_endpoint(
                                            &settings_new_label,
                                            &settings_new_url,
                                            &settings_new_model,
                                            api_key_opt,
                                        ) {
                                            Ok(()) => {
                                                left_committed.push(format!("Added endpoint: {}", settings_new_label));
                                                left_follow_output = true;
                                                left_scroll = 10_000;
                                            }
                                            Err(e) => {
                                                left_committed.push(format!("Failed to save endpoint: {}", e));
                                                left_follow_output = true;
                                                left_scroll = 10_000;
                                            }
                                        }
                                        settings_endpoints = vec![crate::config::InferenceEndpoint::from_config(&config)];
                                        if let Ok(eps) = keystore.decrypt_all_endpoints() {
                                            settings_endpoints.extend(eps);
                                        }
                                        settings_adding = false;
                                        settings_add_step = 0;
                                        settings_edit_buf.clear();
                                        settings_new_label.clear();
                                        settings_new_url.clear();
                                        settings_new_model.clear();
                                        settings_new_key.clear();
                                    }
                                    _ => {}
                                }
                            }
                            KeyCode::Esc => {
                                settings_adding = false;
                                settings_add_step = 0;
                                settings_edit_buf.clear();
                                settings_new_label.clear();
                                settings_new_url.clear();
                                settings_new_model.clear();
                                settings_new_key.clear();
                            }
                            _ => {}
                        }
                    } else {
                        match key.code {
                            KeyCode::Up | KeyCode::Char('k') => {
                                if settings_selected > 0 { settings_selected -= 1; }
                            }
                            KeyCode::Down | KeyCode::Char('j') => {
                                if settings_selected < settings_endpoints.len().saturating_sub(1) {
                                    settings_selected += 1;
                                }
                            }
                            KeyCode::Enter => {
                                if settings_selected != active_endpoint_idx {
                                    let ep = &settings_endpoints[settings_selected];
                                    trace_lines.push(format!("Switching to: {} ({})", ep.label, ep.base_url));
                                    right_follow_output = true;
                                    right_scroll = 10_000;

                                    let probe_result = crate::llm::probe_context_size(
                                        &ep.base_url,
                                        &ep.model,
                                        ep.api_key.as_deref(),
                                    ).await;

                                    let budget = match probe_result {
                                        Some(n_ctx) => {
                                            trace_lines.push(format!("   context: {} tokens (probed)", n_ctx));
                                            crate::config::ContextBudget::from_context_tokens(n_ctx, config.max_rounds)
                                        }
                                        None => {
                                            trace_lines.push("   context probe failed, using default 8192".to_string());
                                            crate::config::ContextBudget::default_fallback()
                                        }
                                    };

                                    if let Ok(mut ag) = agent.try_lock() {
                                        ag.switch_endpoint(ep, budget.clone());
                                    }

                                    // Update display state so status bar reflects the switch
                                    display_model = ep.model.clone();
                                    display_budget = budget;

                                    active_endpoint_idx = settings_selected;
                                    left_committed.push(format!("Switched to: {} ({})", ep.label, ep.model));
                                    left_follow_output = true;
                                    left_scroll = 10_000;
                                }
                                settings_active = false;
                            }
                            KeyCode::Char('a') | KeyCode::Char('A') => {
                                settings_adding = true;
                                settings_add_step = 0;
                                settings_edit_buf.clear();
                                settings_new_label.clear();
                                settings_new_url.clear();
                                settings_new_model.clear();
                                settings_new_key.clear();
                            }
                            KeyCode::Char('d') | KeyCode::Char('D') => {
                                if settings_selected > 0 && settings_selected != active_endpoint_idx {
                                    let keystore_idx = settings_selected - 1;
                                    let _ = keystore.remove_endpoint(keystore_idx);
                                    settings_endpoints = vec![crate::config::InferenceEndpoint::from_config(&config)];
                                    if let Ok(eps) = keystore.decrypt_all_endpoints() {
                                        settings_endpoints.extend(eps);
                                    }
                                    if settings_selected >= settings_endpoints.len() {
                                        settings_selected = settings_endpoints.len().saturating_sub(1);
                                    }
                                    if active_endpoint_idx > settings_selected {
                                        active_endpoint_idx = active_endpoint_idx.saturating_sub(1);
                                    }
                                    left_committed.push("Endpoint deleted.".to_string());
                                    left_follow_output = true;
                                    left_scroll = 10_000;
                                }
                            }
                            KeyCode::Esc => {
                                settings_active = false;
                            }
                            _ => {}
                        }
                    }
                    // Don't fall through to normal input handling
                    // Don't fall through to normal input handling
                }
                if !settings_active {
                match key.code {
                    // --- Slash command menu navigation (takes precedence when active) ---
                    KeyCode::Up if input.starts_with('/') => {
                        let filtered = get_filtered_commands(&slash_commands, &input);
                        if !filtered.is_empty() {
                            slash_selected = slash_selected.saturating_sub(1);
                        }
                    }
                    KeyCode::Down if input.starts_with('/') => {
                        let filtered = get_filtered_commands(&slash_commands, &input);
                        if !filtered.is_empty() {
                            slash_selected = (slash_selected + 1).min(filtered.len().saturating_sub(1));
                        }
                    }
                    KeyCode::Tab if input.starts_with('/') => {
                        let filtered = get_filtered_commands(&slash_commands, &input);
                        if let Some(cmd) = filtered.get(slash_selected.min(filtered.len().saturating_sub(1))) {
                            input = format!("/{} ", cmd.name);
                            clamp_slash_selection(&slash_commands, &input, &mut slash_selected);
                        }
                    }

                    KeyCode::Tab => {
                        focused_pane = match focused_pane {
                            Pane::Left => Pane::Right,
                            Pane::Right => Pane::Left,
                        };
                    }
                    KeyCode::Esc => {
                        if input.starts_with('/') {
                            // Dismiss command menu / clear partial command
                            input.clear();
                            slash_selected = 0;
                        } else {
                            // Release focus on escape
                            focused_pane = Pane::Left;
                            left_follow_output = true;
                            right_follow_output = true;
                        }
                    }
                    KeyCode::PageUp if focused_pane == Pane::Right => { right_follow_output = false; let old_scroll = right_scroll; right_scroll = right_scroll.saturating_sub(8); if old_scroll == right_scroll && old_scroll == 0 { scroll_flash_timer = 10; } }
                    KeyCode::PageUp if focused_pane == Pane::Left => { left_follow_output = false; let old_scroll = left_scroll; left_scroll = left_scroll.saturating_sub(8); if old_scroll == left_scroll && old_scroll == 0 { scroll_flash_timer = 10; } }
                    KeyCode::PageDown if focused_pane == Pane::Right => { let old_scroll = right_scroll; right_scroll = right_scroll.saturating_add(8); let right_max = last_right_line_count.saturating_sub(last_right_area.height.saturating_sub(2)); if old_scroll == right_scroll && old_scroll >= right_max { scroll_flash_timer = 10; } }
                    KeyCode::PageDown if focused_pane == Pane::Left => { let old_scroll = left_scroll; left_scroll = left_scroll.saturating_add(8); let left_max = last_left_line_count.saturating_sub(last_left_area.height.saturating_sub(2)); if old_scroll == left_scroll && old_scroll >= left_max { scroll_flash_timer = 10; } }
                    KeyCode::Up if !mode_menu_active && focused_pane == Pane::Right => { right_follow_output = false; let old_scroll = right_scroll; right_scroll = right_scroll.saturating_sub(1); if old_scroll == right_scroll && old_scroll == 0 { scroll_flash_timer = 10; } }
                    KeyCode::Up if !mode_menu_active && focused_pane == Pane::Left => { left_follow_output = false; let old_scroll = left_scroll; left_scroll = left_scroll.saturating_sub(1); if old_scroll == left_scroll && old_scroll == 0 { scroll_flash_timer = 10; } }
                    KeyCode::Down if !mode_menu_active && focused_pane == Pane::Right => { let old_scroll = right_scroll; right_scroll = right_scroll.saturating_add(1); let right_max = last_right_line_count.saturating_sub(last_right_area.height.saturating_sub(2)); if old_scroll == right_scroll && old_scroll >= right_max { scroll_flash_timer = 10; } }
                    KeyCode::Down if !mode_menu_active && focused_pane == Pane::Left => { let old_scroll = left_scroll; left_scroll = left_scroll.saturating_add(1); let left_max = last_left_line_count.saturating_sub(last_left_area.height.saturating_sub(2)); if old_scroll == left_scroll && old_scroll >= left_max { scroll_flash_timer = 10; } }
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        return Ok(());
                    }
                    KeyCode::Enter => {
                        let prompt = input.trim().to_string();
                        if prompt.is_empty() {
                            // nothing to do
                        } else if prompt.starts_with('/') {
                            // Dispatch slash command (do not send to model)
                            let cmd_part = prompt.trim_start_matches('/');
                            let mut parts = cmd_part.splitn(2, ' ');
                            let name = parts.next().unwrap_or("").to_lowercase();
                            // let _arg = parts.next().unwrap_or("").trim(); // future use for parameterized cmds

                            match name.as_str() {
                                "help" | "?" => {
                                    let help_text = "\
Available commands:
/help          Show this help
/clear         Clear the conversation pane
/clear-trace   Clear the right trace pane
/reset         Reset conversation memory (session goals stay)
/status        Show endpoint, model, workspace
/mode          Change execution approval mode (Babysitter / Spring Break / Vegas / Thunderdome)
/settings      Manage inference endpoints (add/switch/delete)
/quit or /exit Quit the TUI

Tip: type / then use ↑↓ to browse, Tab to complete.".to_string();
                                    left_committed.push(help_text);
                                    left_follow_output = true;
                                    left_scroll = 10_000;
                                    input.clear();
                                    slash_selected = 0;
                                }
                                "clear" => {
                                    if let Ok(mut ag) = agent.try_lock() {
                                        ag.reset();
                                    }
                                    left_committed.clear();
                                    left_committed.push("Conversation cleared.".to_string());
                                    left_follow_output = true;
                                    left_scroll = 10_000;
                                    input.clear();
                                    slash_selected = 0;
                                }
                                "clear-trace" => {
                                    trace_lines.clear();
                                    current_thinking.clear();
                                    input.clear();
                                    slash_selected = 0;
                                }
                                "reset" => {
                                    if let Ok(mut ag) = agent.try_lock() {
                                        ag.reset();
                                    }
                                    left_committed.clear();
                                    left_committed.push("Conversation reset (persistent session kept).".to_string());
                                    left_follow_output = true;
                                    left_scroll = 10_000;
                                    input.clear();
                                    slash_selected = 0;
                                }
                                "status" => {
                                    let mode_label = if let Ok(ag) = agent.try_lock() {
                                        ag.current_exec_mode().label().to_string()
                                    } else {
                                        "unknown".to_string()
                                    };
                                    let status = format!(
                                        "Session status\n  Model:     {}\n  Base URL:  {}\n  Workspace: {}\n  Exec Mode: {}\n  History:   {} entries",
                                        config.model,
                                        config.base_url,
                                        config.workspace.display(),
                                        mode_label,
                                        left_committed.len()
                                    );
                                    left_committed.push(status);
                                    left_follow_output = true;
                                    left_scroll = 10_000;
                                    input.clear();
                                    slash_selected = 0;
                                }
                                "mode" => {
                                    mode_menu_active = true;
                                    // initialize selection to current mode if possible
                                    selected_mode_idx = 0;
                                    if let Ok(ag) = agent.try_lock() {
                                        if let Some(s) = &ag.session {
                                            selected_mode_idx = match s.meta.exec_approval_mode {
                                                crate::session::ExecApprovalMode::Babysitter => 0,
                                                crate::session::ExecApprovalMode::SpringBreak => 1,
                                                crate::session::ExecApprovalMode::Vegas => 2,
                                                crate::session::ExecApprovalMode::Thunderdome => 3,
                                            };
                                        }
                                    }
                                    input.clear();
                                    slash_selected = 0;
                                    left_committed.push("Use ↑/↓ to select execution mode, Enter to confirm, Esc to cancel.".to_string());
                                    left_follow_output = true;
                                    left_scroll = 10_000;
                                }
                                "settings" => {
                                    // Build endpoint list: CLI default + saved
                                    settings_endpoints = vec![crate::config::InferenceEndpoint::from_config(&config)];
                                    settings_endpoints.extend(saved_endpoints.clone());
                                    settings_selected = active_endpoint_idx;
                                    settings_editing = None;
                                    settings_adding = false;
                                    settings_active = true;
                                    input.clear();
                                    slash_selected = 0;
                                }
                                "quit" | "exit" | "q" => {
                                    return Ok(());
                                }
                                _ => {
                                    left_committed.push(format!("⚠ Unknown command: {}. Type /help to list commands.", prompt));
                                    left_follow_output = true;
                                    left_scroll = 10_000;
                                    input.clear();
                                    slash_selected = 0;
                                }
                            }
                        } else {
                            // === Normal user prompt to the agent ===
                            // Commit any previous live response (from last turn) if present
                            if !current_response.trim().is_empty() {
                                left_committed.push(format!("Agent: {}", current_response.trim()));
                            }

                            // New turn: record user prompt on left, clear live + trace for fresh view
                            left_committed.push(format!("You: {}", prompt));
                            left_follow_output = true;
                            left_scroll = 10_000; // auto-scroll to bottom on new user message
                            current_response.clear();
                            trace_lines.clear();
                            current_thinking.clear();
                            trace_lines.push(format!("▶ Starting agent turn for: {}", prompt));
                            trace_lines.push("   (waiting for first response from model...)".to_string());
                            right_follow_output = true;
                            right_scroll = 10_000; // auto-scroll trace pane on new turn start

                            input.clear();
                            slash_selected = 0;
                            is_processing = true;
                            tool_calls_this_turn = 0;
                            turn_rounds = 0;

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
                    KeyCode::Char(c) if !mode_menu_active && pending_approval.is_none() => {
                        input.push(c);
                        clamp_slash_selection(&slash_commands, &input, &mut slash_selected);
                    }
                    KeyCode::Backspace if !mode_menu_active && pending_approval.is_none() => {
                        input.pop();
                        clamp_slash_selection(&slash_commands, &input, &mut slash_selected);
                    }
                    KeyCode::PageUp if !mode_menu_active && key.modifiers.contains(KeyModifiers::SHIFT) => { right_follow_output = false; right_scroll = right_scroll.saturating_sub(12); }
                    KeyCode::PageUp if !mode_menu_active => { left_follow_output = false; left_scroll = left_scroll.saturating_sub(12); }
                    KeyCode::PageDown if !mode_menu_active && key.modifiers.contains(KeyModifiers::SHIFT) => { right_scroll = right_scroll.saturating_add(12); }
                    KeyCode::PageDown if !mode_menu_active => { left_scroll = left_scroll.saturating_add(12); }
                    KeyCode::Up if !mode_menu_active && key.modifiers.contains(KeyModifiers::SHIFT) => { right_follow_output = false; right_scroll = right_scroll.saturating_sub(1); }
                    KeyCode::Up if !mode_menu_active => { left_follow_output = false; left_scroll = left_scroll.saturating_sub(1); }
                    KeyCode::Down if !mode_menu_active && key.modifiers.contains(KeyModifiers::SHIFT) => { right_scroll = right_scroll.saturating_add(1); }
                    KeyCode::Down if !mode_menu_active => { left_scroll = left_scroll.saturating_add(1); }
                    _ => {}
                }
                } // if !settings_active
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
