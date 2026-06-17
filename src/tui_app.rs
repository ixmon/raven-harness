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
    widgets::{Block, BorderType, Borders, Paragraph, Wrap},
    Terminal,
};
use std::io;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

use crate::agent::Agent;
use crate::config::Config;
use crate::llm::StreamChunk;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Pane {
    Left,
    Right,
}

pub async fn run(config: Config) -> Result<()> {
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

    let res = run_app(&mut terminal, config).await;

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

    // Channel for agent to push live updates into the TUI
    let (tx, mut rx) = mpsc::channel::<UiUpdate>(64);

    // Channel for input thread to send key events to main loop
    let (input_tx, mut input_rx) = mpsc::channel::<Event>(64);

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
        // Draw - split into left (main conversation + current turn output) and right (thinking + tool debug)
        terminal.draw(|f| {
            let size = f.area();

            // Vertical: content area + input bar
            let vertical = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(8), Constraint::Length(3)])
                .split(size);

            // Horizontal split for the two panes
            let panes = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(62), Constraint::Percentage(38)])
                .split(vertical[0]);

            let left_area = panes[0];
            let right_area = panes[1];
            last_left_area = left_area;
            last_right_area = right_area;
            let input_area = vertical[1];

            // ---- Left pane: committed history + live current turn output ----
            let mut left_text = Text::default();

            // Gothic raven ASCII art temporarily disabled (still looked a bit vague after uninvert).
            // Assets + generator script are left in place for easy re-enabling later.
            // To turn back on:
            //   1. Uncomment the block below (or restore from git).
            //   2. Optionally tweak shades or re-run: ./scripts/generate_raven.sh <source.png>
            //
            // let raven_art = include_str!("../assets/raven.txt");
            // for line in raven_art.lines() {
            //     ... (shade mapping + left_text.lines.push ...)
            // }
            // left_text.lines.push(Line::from(""));  // breathing room before banner

            // (no art for now)

            // Real conversation content
            for (i, entry) in left_committed.iter().enumerate() {
                for line in entry.lines() {
                    left_text.lines.push(Line::from(line.to_string()));
                }
                if i < left_committed.len() - 1 {
                    left_text.lines.push(Line::from(""));
                }
            }

            if !current_response.is_empty() {
                left_text.lines.push(Line::from(Span::styled(
                    "Agent (current turn):",
                    Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
                )));
                for line in current_response.lines() {
                    left_text.lines.push(Line::from(line.to_string()));
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
                Style::default().fg(Color::Gray)
            };
            let left_block = Paragraph::new(left_text)
                .block(
                    Block::default()
                        .title("Conversation")
                        .borders(Borders::ALL)
                        .border_type(BorderType::Rounded)
                        .border_style(focus_style)
                        .title_style(Style::default().fg(Color::Gray)),
                )
                .wrap(Wrap { trim: false })
                .scroll((left_scroll, 0));

            f.render_widget(left_block, left_area);

            // Simple scrollbar indicator on the right edge of the conversation pane
            render_scrollbar::<B>(f, left_area, left_scroll, left_line_count);

            // ---- Right pane: thinking + tool call trace ----
            let mut right_text = Text::default();

            // Real trace content (starting marker, thinking, tool calls)
            for line in &trace_lines {
                right_text.lines.push(Line::from(line.clone()));
            }

            // Show any live accumulating thinking that hasn't hit a boundary yet
            if !current_thinking.is_empty() {
                right_text.lines.push(Line::from(format!("🧠 {}", current_thinking.trim())));
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
                Style::default().fg(Color::Gray)
            };
            let right_block = Paragraph::new(right_text)
                .block(
                    Block::default()
                        .title("Trace (thinking + tools)")
                        .borders(Borders::ALL)
                        .border_type(BorderType::Rounded)
                        .border_style(right_focus_style)
                        .title_style(Style::default().fg(Color::Gray)),
                )
                .wrap(Wrap { trim: false })
                .scroll((right_scroll, 0));

            f.render_widget(right_block, right_area);

            // Simple scrollbar for trace pane
            render_scrollbar::<B>(f, right_area, right_scroll, right_line_count);

            // Input bar (full width at bottom)
            let input_title = if is_processing {
                "Input (processing... Esc to scroll, Ctrl-C to quit)"
            } else {
                "Input (Enter to send, Ctrl-C to quit)"
            };
            let input_para = Paragraph::new(input.as_str())
                .style(Style::default().fg(Color::White))
                .block(
                    Block::default()
                        .title(input_title)
                        .borders(Borders::ALL)
                        .border_type(BorderType::Rounded)
                        .border_style(Style::default().fg(Color::Rgb(0x99, 0x99, 0x99)))
                        .title_style(Style::default().fg(Color::Rgb(0x99, 0x99, 0x99))),
                );
            f.render_widget(input_para, input_area);

            // Cursor in input
            f.set_cursor_position((input_area.x + 1 + input.len() as u16, input_area.y + 1));
            
            // Decrement scroll flash timer
            if scroll_flash_timer > 0 {
                scroll_flash_timer = scroll_flash_timer.saturating_sub(1);
            }
        })?;

        // Handle input + agent updates (non-blocking)
        if is_processing {
            // While processing we mostly listen for agent updates and a few keys
            tokio::select! {
                Some(update) = rx.recv() => {
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
                    }
                }

                // Allow the user to scroll AND type-ahead while processing
                Some(ev) = input_rx.recv() => {
                    if let Event::Key(key) = ev {
                        match key.code {
                            KeyCode::Tab => {
                                focused_pane = match focused_pane {
                                    Pane::Left => Pane::Right,
                                    Pane::Right => Pane::Left,
                                };
                                continue; // Force redraw to show focus change
                            }
                            KeyCode::Esc => { 
                                // Release focus on escape
                                focused_pane = Pane::Left; 
                                left_follow_output = true;
                                right_follow_output = true;
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
                            KeyCode::Char(c) => { input.push(c); }
                            KeyCode::Backspace => { input.pop(); }
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
                match key.code {
                    KeyCode::Tab => {
                        focused_pane = match focused_pane {
                            Pane::Left => Pane::Right,
                            Pane::Right => Pane::Left,
                        };
                    }
                    KeyCode::Esc => { 
                        // Release focus on escape
                        focused_pane = Pane::Left; 
                        left_follow_output = true;
                        right_follow_output = true;
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
                    KeyCode::Enter => {
                        if !input.trim().is_empty() {
                            let prompt = input.trim().to_string();

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
                            is_processing = true;

                            // Spawn the agent turn — agent is behind Arc<Mutex> so it
                            // persists across turns with full conversation history.
                            let tx2 = tx.clone();
                            let agent_clone = agent.clone();
                            let prompt2 = prompt.clone();
                            let max_rounds = config.max_rounds.min(12);

                            tokio::spawn(async move {
                                let mut agent = agent_clone.lock().await;

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

                                        // Execute tool calls and report to UI
                                        for tc in &tool_calls {
                                            let _ = tx2.send(UiUpdate::ToolStart {
                                                name: tc.function.name.clone(),
                                                args: tc.function.arguments.clone(),
                                            }).await;
                                        }

                                        let records = agent
                                            .execute_and_record_tool_calls(&tool_calls)
                                            .await;

                                        for r in records {
                                            let _ = tx2.send(UiUpdate::ToolResult {
                                                name: r.tool,
                                                summary: r.summary,
                                            }).await;
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

                                let _ = tx2.send(UiUpdate::Done { final_text }).await;
                            });
                        }
                    }
                    KeyCode::Char(c) => {
                        input.push(c);
                    }
                    KeyCode::Backspace => {
                        input.pop();
                    }
                    KeyCode::Esc => {
                        input.clear();
                    }
                    KeyCode::PageUp if key.modifiers.contains(KeyModifiers::SHIFT) => { right_follow_output = false; right_scroll = right_scroll.saturating_sub(12); }
                    KeyCode::PageUp => { left_follow_output = false; left_scroll = left_scroll.saturating_sub(12); }
                    KeyCode::PageDown if key.modifiers.contains(KeyModifiers::SHIFT) => { right_scroll = right_scroll.saturating_add(12); }
                    KeyCode::PageDown => left_scroll = left_scroll.saturating_add(12),
                    KeyCode::Up if key.modifiers.contains(KeyModifiers::SHIFT) => { right_follow_output = false; right_scroll = right_scroll.saturating_sub(1); }
                    KeyCode::Up => { left_follow_output = false; left_scroll = left_scroll.saturating_sub(1); }
                    KeyCode::Down if key.modifiers.contains(KeyModifiers::SHIFT) => { right_scroll = right_scroll.saturating_add(1); }
                    KeyCode::Down => left_scroll = left_scroll.saturating_add(1),
                    _ => {}
                }
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
}

/// Render a minimal scrollbar on the right edge of a pane.
fn render_scrollbar<B: ratatui::backend::Backend>(
    f: &mut ratatui::Frame,
    area: ratatui::layout::Rect,
    scroll: u16,
    total_lines: u16,
) {
    let content_height = area.height.saturating_sub(2);
    if total_lines <= content_height || content_height == 0 {
        return;
    }
    let thumb_height = ((content_height as f32 * content_height as f32 / total_lines as f32) as u16).max(1);
    let scroll_range = total_lines.saturating_sub(content_height);
    let thumb_offset = if scroll_range > 0 {
        (scroll as f32 * (content_height - thumb_height) as f32 / scroll_range as f32) as u16
    } else {
        0
    };
    let buf = f.buffer_mut();
    for i in 0..content_height {
        let y = area.y + 1 + i;
        let x = area.x + area.width - 1;
        let is_thumb = i >= thumb_offset && i < thumb_offset + thumb_height;
        let ch = if is_thumb { "█" } else { "│" };
        let color = if is_thumb {
            Color::Rgb(0xb0, 0xb0, 0xb0)
        } else {
            Color::Gray
        };
        buf.set_string(x, y, ch, Style::default().fg(color));
    }
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
