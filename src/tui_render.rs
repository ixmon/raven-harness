//! Extracted TUI rendering helpers (glm.md refactor).

use crate::input_dispatch::SlashCommand;
use crate::settings_modal::{draw_settings_modal, SettingsModal};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, LineGauge, Paragraph, Wrap},
    Frame,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Pane {
    #[default]
    Left,
    Right,
}

pub struct StatusBarData<'a> {
    pub display_model: &'a str,
    pub ctx_used_tokens: u32,
    pub budget: &'a crate::config::ContextBudget,
    pub mode_label: &'a str,
    pub goal_text: &'a str,
    pub search_label: &'a str,
}

pub fn draw_status_bar(f: &mut Frame, area: Rect, data: &StatusBarData<'_>) {
    let ctx = data.budget;
    let mut spans = vec![
        Span::styled(" ⦖ ", Style::default().fg(Color::Rgb(0xc0, 0x80, 0xff))),
        Span::styled(
            data.display_model,
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ),
        Span::styled("  │  ", Style::default().fg(Color::DarkGray)),
        Span::styled("ctx:", Style::default().fg(Color::DarkGray)),
        Span::styled(
            {
                let used_k = data.ctx_used_tokens / 1000;
                let max_k = ctx.context_tokens / 1000;
                format!("{}k/{}k", used_k, max_k)
            },
            {
                let ratio = data.ctx_used_tokens as f64 / ctx.context_tokens.max(1) as f64;
                if ratio < 0.5 {
                    Style::default().fg(Color::Rgb(0x80, 0xd0, 0x80))
                } else if ratio < 0.8 {
                    Style::default().fg(Color::Rgb(0xff, 0xc0, 0x40))
                } else {
                    Style::default().fg(Color::Rgb(0xff, 0x60, 0x60))
                }
            },
        ),
        Span::styled(
            format!("({})", ctx.source),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled("  │  ", Style::default().fg(Color::DarkGray)),
        Span::styled("mode:", Style::default().fg(Color::DarkGray)),
        Span::styled(
            data.mode_label.split(" - ").next().unwrap_or("?"),
            Style::default().fg(Color::Yellow),
        ),
        Span::styled("  │  ", Style::default().fg(Color::DarkGray)),
        Span::styled("goal:", Style::default().fg(Color::DarkGray)),
        Span::styled(
            truncate_str(data.goal_text, 40),
            Style::default().fg(Color::Rgb(0xa0, 0xd0, 0xff)),
        ),
    ];
    if !data.search_label.is_empty() {
        spans.push(Span::styled("  │  ", Style::default().fg(Color::DarkGray)));
        spans.push(Span::styled(
            data.search_label,
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

pub struct ContextGaugeData {
    pub turn_rounds: usize,
    pub max_rounds: u32,
    pub tool_calls_this_turn: usize,
}

pub fn draw_context_gauge(f: &mut Frame, area: Rect, data: &ContextGaugeData) {
    let max_rounds = data.max_rounds.min(12) as f64;
    let ratio = (data.turn_rounds as f64 / max_rounds).min(1.0);
    let gauge_label = format!(
        " round {}/{} • {} tool calls",
        data.turn_rounds, data.max_rounds.min(12), data.tool_calls_this_turn
    );
    let gauge_color = if ratio < 0.5 {
        Color::Rgb(0x60, 0xd0, 0x80)
    } else if ratio < 0.8 {
        Color::Rgb(0xff, 0xc0, 0x40)
    } else {
        Color::Rgb(0xff, 0x60, 0x60)
    };
    let gauge = LineGauge::default()
        .ratio(ratio)
        .label(Line::from(Span::styled(
            gauge_label,
            Style::default().fg(Color::White),
        )))
        .filled_style(Style::default().fg(gauge_color))
        .unfilled_style(Style::default().fg(Color::Rgb(0x33, 0x33, 0x44)))
        .line_set(ratatui::symbols::line::THICK);
    f.render_widget(gauge, area);
}

pub struct InputBarData<'a> {
    pub input: &'a str,
    pub is_processing: bool,
    pub spinner_tick: usize,
    pub search_mode: bool,
    pub focused: bool,
}

pub fn draw_input_bar(f: &mut Frame, area: Rect, data: &InputBarData<'_>) {
    let spinner_frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let input_title = if data.search_mode {
        Line::from(vec![
            Span::styled(
                " 🔍 ",
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("Search", Style::default().fg(Color::Magenta)),
            Span::styled(
                "  Enter find • n/N next/prev • Esc cancel ",
                Style::default().fg(Color::DarkGray),
            ),
        ])
    } else if data.input.starts_with('/') {
        Line::from(vec![
            Span::styled(
                " / ",
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
            Span::styled("Commands", Style::default().fg(Color::Gray)),
            Span::styled(
                "  ↑↓ select • Tab complete • Enter run • Esc clear ",
                Style::default().fg(Color::DarkGray),
            ),
        ])
    } else if data.is_processing {
        let frame = spinner_frames[data.spinner_tick % spinner_frames.len()];
        Line::from(vec![
            Span::styled(
                format!(" {} ", frame),
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
            Span::styled("Processing", Style::default().fg(Color::Cyan)),
            Span::styled(
                "  Esc",
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" to STOP  ", Style::default().fg(Color::DarkGray)),
            Span::styled("Ctrl-C", Style::default().fg(Color::Red)),
            Span::styled(" quit ", Style::default().fg(Color::DarkGray)),
        ])
    } else {
        Line::from(vec![
            Span::styled(
                " > ",
                Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
            ),
            Span::styled("Input", Style::default().fg(Color::Gray)),
            Span::styled(
                "  Enter send • Ctrl-J newline • Ctrl-F search • ",
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(
                "/",
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" commands ", Style::default().fg(Color::DarkGray)),
        ])
    };
    let border_style = if data.focused {
        Style::default().fg(Color::White)
    } else {
        Style::default().fg(Color::Rgb(0x55, 0x55, 0x66))
    };
    let input_para = Paragraph::new(data.input)
        .style(Style::default().fg(Color::White))
        .block(
            Block::default()
                .title(input_title)
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(border_style),
        )
        .wrap(Wrap { trim: false });
    f.render_widget(input_para, area);
}

pub fn draw_slash_menu(
    f: &mut Frame,
    input_area: Rect,
    commands: &[SlashCommand],
    input: &str,
    slash_selected: usize,
) {
    let filtered = crate::input_dispatch::filtered_slash_commands(commands, input);
    if filtered.is_empty() {
        return;
    }

    let max_visible = 7usize;
    let visible = filtered.len().min(max_visible);
    let extra = if filtered.len() > max_visible { 1 } else { 0 };
    let menu_h = (visible as u16) + 1 + extra + 2;

    let menu_area = Rect {
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
        let mut spans = vec![
            Span::styled(
                marker,
                if is_selected {
                    Style::default().fg(Color::Cyan)
                } else {
                    Style::default().fg(Color::Gray)
                },
            ),
            Span::styled(format!("/{}", cmd.name), name_style),
        ];
        if !cmd.desc.is_empty() {
            spans.push(Span::styled(
                format!("  — {}", cmd.desc),
                Style::default().fg(Color::DarkGray),
            ));
        }
        menu_text.lines.push(Line::from(spans));
    }

    if filtered.len() > max_visible {
        menu_text.lines.push(Line::from(Span::styled(
            "   …",
            Style::default().fg(Color::DarkGray),
        )));
    }

    let menu_block = Paragraph::new(menu_text).block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(Color::Rgb(0x55, 0x55, 0x55))),
    );
    f.render_widget(menu_block, menu_area);
}

pub fn draw_mode_menu(
    f: &mut Frame,
    input_area: Rect,
    approval_modes: &[&str],
    selected_mode_idx: usize,
) {
    let desired_h = 1u16 + approval_modes.len() as u16 + 2;
    let menu_h = if input_area.y >= desired_h {
        desired_h
    } else {
        input_area.y.max(4)
    };
    let menu_area = Rect {
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
        menu_text
            .lines
            .push(Line::from(Span::styled(format!("{}{}", marker, m), style)));
    }

    let menu_block = Paragraph::new(menu_text).block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(Color::Rgb(0x55, 0x55, 0x55))),
    );
    f.render_widget(menu_block, menu_area);
}

pub fn draw_approval_popup(f: &mut Frame, desc: &str, input_area: Rect) {
    let pw = 60u16;
    let ph = 9u16;
    let px = 2;
    let py = input_area.y.saturating_sub(ph + 1);
    let pa = Rect::new(px, py, pw, ph);
    let safe_desc = truncate_str(desc, 220);
    let popup_text = Text::from(vec![
        Line::from(Span::styled(
            "Sandbox approval needed",
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(safe_desc, Style::default().fg(Color::White))),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                "[Y]",
                Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
            ),
            Span::styled("es  ", Style::default().fg(Color::Gray)),
            Span::styled(
                "[N]",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Span::styled("o (Esc)", Style::default().fg(Color::Gray)),
        ]),
    ]);
    let popup = Paragraph::new(popup_text)
        .style(Style::default().fg(Color::Yellow))
        .wrap(Wrap { trim: true })
        .block(
            Block::default()
                .title("Action Approval")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow)),
        );
    f.render_widget(popup, pa);
}

pub fn draw_left_pane(
    f: &mut Frame,
    left_committed: &[String],
    current_response: &str,
    left_area: Rect,
    last_left_area: &mut Rect,
    last_left_line_count: &mut u16,
    left_follow_output: bool,
    left_scroll: &mut u16,
    focused_pane: Pane,
    scroll_flash_timer: u8,
    highlight_line: Option<usize>,
) {
    *last_left_area = left_area;

    let mut left_text = Text::default();
    let mut line_idx = 0usize;

    for (i, entry) in left_committed.iter().enumerate() {
        let (prefix_style, body_style) = conversation_entry_styles(entry);

        let lines_iter: Vec<&str> = entry.lines().collect();
        for (li, line) in lines_iter.iter().enumerate() {
            let style = if Some(line_idx) == highlight_line {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Magenta)
                    .add_modifier(Modifier::BOLD)
            } else if li == 0 {
                prefix_style
            } else {
                body_style
            };
            left_text
                .lines
                .push(Line::from(Span::styled(line.to_string(), style)));
            line_idx += 1;
        }
        if i < left_committed.len() - 1 {
            left_text.lines.push(Line::from(""));
            line_idx += 1;
        }
    }

    if !current_response.is_empty() {
        left_text.lines.push(Line::from(Span::styled(
            "Agent (streaming):",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD | Modifier::ITALIC),
        )));
        line_idx += 1;

        let lines: Vec<&str> = current_response.lines().collect();
        let has_partial = !current_response.ends_with('\n');
        let complete_count = if has_partial {
            lines.len().saturating_sub(1)
        } else {
            lines.len()
        };

        for line in &lines[..complete_count] {
            let style = if Some(line_idx) == highlight_line {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Magenta)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Rgb(0xd0, 0xf0, 0xd0))
            };
            left_text
                .lines
                .push(Line::from(Span::styled(line.to_string(), style)));
            line_idx += 1;
        }

        if has_partial {
            if let Some(partial) = lines.last() {
                left_text.lines.push(Line::from(Span::styled(
                    partial.to_string(),
                    Style::default()
                        .fg(Color::Rgb(0xd0, 0xf0, 0xd0))
                        .add_modifier(Modifier::ITALIC),
                )));
            }
        }
    }

    render_scrollable_pane(
        f,
        left_area,
        &mut left_text,
        last_left_line_count,
        left_follow_output,
        left_scroll,
        focused_pane == Pane::Left,
        scroll_flash_timer,
        "  Conversation",
        Color::Cyan,
        Some(format!("  ({} msgs)", left_committed.len())),
    );
}

pub fn draw_right_pane(
    f: &mut Frame,
    trace_lines: &[String],
    current_thinking: &str,
    right_area: Rect,
    last_right_area: &mut Rect,
    last_right_line_count: &mut u16,
    right_follow_output: bool,
    right_scroll: &mut u16,
    focused_pane: Pane,
    scroll_flash_timer: u8,
    highlight_line: Option<usize>,
) {
    *last_right_area = right_area;

    let mut right_text = Text::default();
    let mut line_idx = 0usize;

    for line in trace_lines {
        let style = if Some(line_idx) == highlight_line {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Magenta)
                .add_modifier(Modifier::BOLD)
        } else {
            trace_line_style(line)
        };
        right_text
            .lines
            .push(Line::from(Span::styled(line.clone(), style)));
        line_idx += 1;
    }

    if !current_thinking.is_empty() {
        if !trace_lines.is_empty() {
            right_text.lines.push(Line::from(""));
            line_idx += 1;
        }
        right_text.lines.push(Line::from(Span::styled(
            "Thinking (live):",
            Style::default()
                .fg(Color::LightCyan) // ANSI16: LightCyan, 256-color: 14, TrueColor: #00e5e5
                .add_modifier(Modifier::BOLD | Modifier::ITALIC),
        )));
        line_idx += 1;
        for line in current_thinking.lines() {
            let style = if Some(line_idx) == highlight_line {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Magenta)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .fg(Color::LightCyan)
                    .add_modifier(Modifier::ITALIC)
            };
            right_text
                .lines
                .push(Line::from(Span::styled(line.to_string(), style)));
            line_idx += 1;
        }
    }

    render_scrollable_pane(
        f,
        right_area,
        &mut right_text,
        last_right_line_count,
        right_follow_output,
        right_scroll,
        focused_pane == Pane::Right,
        scroll_flash_timer,
        "  Trace",
        Color::Rgb(0xd0, 0xa0, 0xff),
        None,
    );
}

pub fn draw_overlays(
    f: &mut Frame,
    screen: Rect,
    input_area: Rect,
    settings: &SettingsModal,
    pending_approval: Option<&str>,
    slash_commands: &[SlashCommand],
    input: &str,
    slash_selected: usize,
    mode_menu_active: bool,
    approval_modes: &[&str],
    selected_mode_idx: usize,
) {
    if let Some(desc) = pending_approval {
        draw_approval_popup(f, desc, input_area);
    }
    if input.starts_with('/') && !input.is_empty() {
        draw_slash_menu(f, input_area, slash_commands, input, slash_selected);
    }
    if mode_menu_active {
        draw_mode_menu(f, input_area, approval_modes, selected_mode_idx);
    }
    draw_settings_modal(f, screen, settings);
}

fn conversation_entry_styles(entry: &str) -> (Style, Style) {
    if entry.starts_with("You: ") {
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
    } else if entry.starts_with("✅")
        || entry.starts_with("⛔")
        || entry.starts_with("⏹")
        || entry.starts_with("🔒")
    {
        (Style::default().fg(Color::Yellow), Style::default().fg(Color::Yellow))
    } else {
        (
            Style::default().fg(Color::Rgb(0x88, 0x88, 0xaa)),
            Style::default().fg(Color::Rgb(0x88, 0x88, 0xaa)),
        )
    }
}

fn trace_line_style(line: &str) -> Style {
    if line.starts_with("🧠") {
        Style::default()
            .fg(Color::Rgb(0xd0, 0xa0, 0xff))
            .add_modifier(Modifier::ITALIC)
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
    } else {
        Style::default().fg(Color::Rgb(0xcc, 0xcc, 0xdd))
    }
}

fn render_scrollable_pane(
    f: &mut Frame,
    area: Rect,
    text: &mut Text,
    last_line_count: &mut u16,
    follow_output: bool,
    scroll: &mut u16,
    focused: bool,
    scroll_flash_timer: u8,
    title: &str,
    title_color: Color,
    subtitle: Option<String>,
) {
    let line_count = text.lines.len() as u16;
    *last_line_count = line_count;
    let content_height = area.height.saturating_sub(2);
    if follow_output {
        *scroll = line_count.saturating_sub(content_height);
    }
    *scroll = (*scroll).min(line_count.saturating_sub(1));

    let focus_style = if focused {
        if scroll_flash_timer > 0 {
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        }
    } else {
        Style::default().fg(Color::Rgb(0x55, 0x55, 0x66))
    };

    let title_line = if let Some(sub) = subtitle {
        Line::from(vec![
            Span::styled(title, Style::default().fg(title_color).add_modifier(Modifier::BOLD)),
            Span::styled(sub, Style::default().fg(Color::DarkGray)),
        ])
    } else {
        Line::from(Span::styled(
            title,
            Style::default().fg(title_color).add_modifier(Modifier::BOLD),
        ))
    };

    let block = Paragraph::new(text.clone())
        .block(
            Block::default()
                .title(title_line)
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(focus_style)
                .padding(ratatui::widgets::Padding::new(1, 1, 0, 0)),
        )
        .wrap(Wrap { trim: false })
        .scroll((*scroll, 0));

    f.render_widget(block, area);

    if line_count > content_height {
        use ratatui::widgets::{Scrollbar, ScrollbarOrientation, ScrollbarState};
        let mut sb_state =
            ScrollbarState::new(line_count as usize).position((*scroll) as usize);
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None);
        f.render_stateful_widget(scrollbar, area, &mut sb_state);
    }
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let end = (0..=max)
        .rev()
        .find(|&i| s.is_char_boundary(i))
        .unwrap_or(0);
    format!("{}…", &s[..end])
}