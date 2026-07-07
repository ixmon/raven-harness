//! Extracted TUI rendering helpers (glm.md refactor).

#![allow(clippy::too_many_arguments)]

use crate::desktop::{ActiveDesktop, DesktopState, SlideDirection};
use crate::markdown_pane::{highlight_markdown_viewport, markdown_viewport, nav_highlight_search};
use crate::md_render::render_markdown;
use crate::plan_pane_render::draw_plan_pane;
use crate::plan_state::PlanState;
use crate::wiki_doc::{NavItemKind, WikiLink, WikiNavItem};
use crate::input_dispatch::SlashCommand;
use crate::settings_modal::{draw_settings_modal, SettingsModal};
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Clear, LineGauge, Paragraph, Widget, Wrap},
    layout::Alignment,
    Frame,
};

const PLAN_ORANGE: Color = Color::Rgb(0xff, 0xc0, 0x40);

fn is_plan_entry_confirm(entry: &str) -> bool {
    let t = entry.trim();
    t.starts_with("Enter plan mode?")
}

fn push_attention_entry(
    left_text: &mut Text<'static>,
    entry: &str,
    highlight_line: Option<usize>,
    line_idx: &mut usize,
    fg: Color,
) {
    let style = if Some(*line_idx) == highlight_line {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Magenta)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(fg).add_modifier(Modifier::BOLD)
    };
    for line in entry.lines() {
        left_text
            .lines
            .push(Line::from(Span::styled(line.to_string(), style)));
        *line_idx += 1;
    }
}

// markdown rendering: see md_render.rs

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Pane {
    #[default]
    Left,
    Right,
}

pub struct StatusBarData<'a> {
    pub display_model: &'a str,
    pub balance_label: &'a str,
    pub ctx_used_tokens: u32,
    pub budget: &'a raven_tui::config::ContextBudget,
    pub mode_label: &'a str,
    pub agent_mode: &'a str,
    pub goal_text: &'a str,
    pub search_label: &'a str,
    pub tps: f64,
}

pub fn draw_status_bar(f: &mut Frame, area: Rect, data: &StatusBarData<'_>) {
    let ctx = data.budget;
    let mut spans = vec![
        Span::styled(" ⦖ ", Style::default().fg(Color::Rgb(0xc0, 0x80, 0xff))),
        Span::styled(
            data.display_model,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("  ", Style::default()),
        Span::styled(data.balance_label, balance_label_style(data.balance_label)),
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
        Span::styled("  │  ", Style::default().fg(Color::DarkGray)),
        Span::styled("tps:", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("{:.1}", data.tps),
            Style::default().fg(Color::Rgb(0x80, 0xd0, 0x80)),
        ),
        Span::styled("  │  ", Style::default().fg(Color::DarkGray)),
        Span::styled("Approval:", Style::default().fg(Color::DarkGray)),
        Span::styled(
            data.mode_label.split(" - ").next().unwrap_or("?"),
            Style::default().fg(Color::Yellow),
        ),
        Span::styled("  │  ", Style::default().fg(Color::DarkGray)),
        Span::styled("Run Mode:", Style::default().fg(Color::DarkGray)),
        Span::styled(
            data.agent_mode,
            Style::default().fg(Color::Rgb(0xa0, 0xd0, 0xff)),
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

fn balance_label_style(label: &str) -> Style {
    if label == "$∞" {
        Style::default()
            .fg(Color::Rgb(0x80, 0xd0, 0x80))
            .add_modifier(Modifier::BOLD)
    } else if label == "$—" {
        Style::default().fg(Color::DarkGray)
    } else if let Some(amount) = label.strip_prefix('$').and_then(|s| s.parse::<f64>().ok()) {
        let color = if amount < 1.0 {
            Color::Rgb(0xff, 0x60, 0x60)
        } else if amount < 5.0 {
            Color::Rgb(0xff, 0xc0, 0x40)
        } else {
            Color::Rgb(0x80, 0xd0, 0x80)
        };
        Style::default().fg(color).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Rgb(0x80, 0xd0, 0x80))
    }
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
        data.turn_rounds,
        data.max_rounds.min(12),
        data.tool_calls_this_turn
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
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
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
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("Processing", Style::default().fg(Color::Cyan)),
            Span::styled(
                " Enter",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" queue  ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                "Ctrl+Enter",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" now  ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                "Esc",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" stop", Style::default().fg(Color::DarkGray)),
        ])
    } else {
        Line::from(vec![
            Span::styled(
                " > ",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("Input", Style::default().fg(Color::Gray)),
            Span::styled(
                "  Enter send • Ctrl-J newline • Ctrl-F search • ",
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(
                "/",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
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
        Style::default()
            .fg(Color::Gray)
            .add_modifier(Modifier::BOLD),
    )));

    for (i, cmd) in filtered.iter().enumerate().take(max_visible) {
        let is_selected = i == sel;
        let marker = if is_selected { "▶ " } else { "  " };
        let name_style = if is_selected {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
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
            .border_style(Style::default().fg(Color::Rgb(0x55, 0x55, 0x55)))
            .style(Style::default().bg(Color::Black)),
    );
    f.render_widget(Clear, menu_area);
    f.render_widget(menu_block, menu_area);
}

pub fn draw_mode_menu(
    f: &mut Frame,
    input_area: Rect,
    modes: &[&str],
    selected_idx: usize,
    title: &str,
) {
    let desired_h = 1u16 + modes.len() as u16 + 2;
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
        format!("  {}", title),
        Style::default()
            .fg(Color::Gray)
            .add_modifier(Modifier::BOLD),
    )));

    for (i, m) in modes.iter().enumerate() {
        let is_sel = i == selected_idx;
        let marker = if is_sel { "▶ " } else { "  " };
        let style = if is_sel {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
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
            .border_style(Style::default().fg(Color::Rgb(0x55, 0x55, 0x55)))
            .style(Style::default().bg(Color::Black)),
    );
    f.render_widget(Clear, menu_area);
    f.render_widget(menu_block, menu_area);
}

pub fn draw_confirmation_modal(
    f: &mut Frame,
    dialog: &crate::confirmation_dialog::ConfirmationDialog,
    screen: Rect,
    input_area: Rect,
) {
    let view = dialog.view();
    let modal_w = screen.width.saturating_sub(4).clamp(44, 64);
    let inner_w = modal_w.saturating_sub(4) as usize;
    let available_h = input_area.y.saturating_sub(screen.y + 1).max(7);
    let max_desc_lines = ((available_h as usize).saturating_sub(5)).clamp(1, 4);
    let detail_lines = wrap_approval_lines(&view.detail, inner_w, max_desc_lines);

    let body_lines = 1 + detail_lines.len() + 1 + 1;
    let modal_h = (body_lines as u16 + 2).clamp(7, available_h);

    let modal_x = screen.x + (screen.width.saturating_sub(modal_w)) / 2;
    let modal_y = input_area.y.saturating_sub(modal_h + 1).max(screen.y + 1);
    let modal_area = Rect::new(modal_x, modal_y, modal_w, modal_h);

    let detail_style = Style::default().fg(Color::Rgb(0xaa, 0xaa, 0xaa));
    let mut headline_spans = vec![Span::styled(
        view.headline.as_str(),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )];
    if !view.headline_suffix.is_empty() {
        headline_spans.push(Span::styled(
            view.headline_suffix,
            Style::default().fg(Color::DarkGray),
        ));
    }
    let mut popup_lines = vec![Line::from(headline_spans)];
    for line in detail_lines {
        popup_lines.push(Line::from(Span::styled(line, detail_style)));
    }
    popup_lines.push(Line::from(""));
    popup_lines.push(Line::from(vec![
        Span::styled(
            "[Y]",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("es  ", Style::default().fg(Color::Gray)),
        Span::styled(
            "[N]",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        Span::styled("o (Esc)", Style::default().fg(Color::Gray)),
    ]));

    let popup = Paragraph::new(Text::from(popup_lines)).block(
        Block::default()
            .title(Span::styled(
                view.title,
                Style::default()
                    .fg(view.border_color)
                    .add_modifier(Modifier::BOLD),
            ))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(view.border_color))
            .padding(ratatui::widgets::Padding::new(1, 1, 0, 0)),
    );
    f.render_widget(Clear, modal_area);
    f.render_widget(popup, modal_area);
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
    draw_frame: bool,
) {
    *last_left_area = left_area;

    let mut left_text = Text::default();
    let mut line_idx = 0usize;
    let mut consecutive_blanks = 0usize;

    for (i, entry) in left_committed.iter().enumerate() {
        if is_plan_entry_confirm(entry) {
            push_attention_entry(&mut left_text, entry, highlight_line, &mut line_idx, PLAN_ORANGE);
        } else if entry.starts_with("You: ") || entry.starts_with("> ") || entry.starts_with("You (interject") {
            let (prefix_style, body_style) = conversation_entry_styles(entry);
            let lines_iter: Vec<&str> = entry.lines().collect();
            for (li, line) in lines_iter.iter().enumerate() {
                let is_blank = line.trim().is_empty();
                if is_blank {
                    consecutive_blanks += 1;
                    if consecutive_blanks > 2 {
                        continue;
                    }
                } else {
                    consecutive_blanks = 0;
                }
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
        } else {
            // Rich markdown rendering for assistant and system-ish messages.
            // This preserves structure/newlines for plans, lists, code, headings, etc.
            // (User messages stay plain for chat flow.)
            let md = render_markdown(entry);
            for mut line in md.lines {
                if Some(line_idx) == highlight_line {
                    for sp in &mut line.spans {
                        sp.style = sp.style
                            .fg(Color::Black)
                            .bg(Color::Magenta)
                            .add_modifier(Modifier::BOLD);
                    }
                }
                let is_blank = line.spans.is_empty() || line.spans.iter().all(|s| s.content.trim().is_empty());
                if is_blank {
                    consecutive_blanks += 1;
                    if consecutive_blanks > 2 { /* keep a couple for md separation */ }
                    else { left_text.lines.push(line); }
                } else {
                    consecutive_blanks = 0;
                    left_text.lines.push(line);
                }
                line_idx += 1;
            }
        }
        if i < left_committed.len() - 1 && consecutive_blanks < 2 {
            left_text.lines.push(Line::from(""));
            line_idx += 1;
            consecutive_blanks += 1;
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

    let title_str = if draw_frame { "  Conversation" } else { "" };
    let subtitle = if draw_frame {
        Some(format!("  ({} msgs)", left_committed.len()))
    } else {
        None
    };
    render_scrollable_pane(
        f,
        left_area,
        &mut left_text,
        last_left_line_count,
        left_follow_output,
        left_scroll,
        focused_pane == Pane::Left,
        scroll_flash_timer,
        title_str,
        Color::Cyan,
        subtitle,
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
    pending_confirmation: Option<&crate::confirmation_dialog::ConfirmationDialog>,
    slash_commands: &[SlashCommand],
    input: &str,
    slash_selected: usize,
    mode_menu_active: bool,
    approval_modes: &[&str],
    selected_mode_idx: usize,
    agent_mode_menu_active: bool,
    agent_modes: &[&str],
    selected_agent_mode_idx: usize,
) {
    if let Some(dialog) = pending_confirmation {
        draw_confirmation_modal(f, dialog, screen, input_area);
    }
    if input.starts_with('/') && !input.is_empty() {
        draw_slash_menu(f, input_area, slash_commands, input, slash_selected);
    }
    if mode_menu_active {
        draw_mode_menu(f, input_area, approval_modes, selected_mode_idx, "Approval Mode");
    }
    if agent_mode_menu_active {
        draw_mode_menu(f, input_area, agent_modes, selected_agent_mode_idx, "Run Mode");
    }
    draw_settings_modal(f, screen, settings);
}

fn conversation_entry_styles(entry: &str) -> (Style, Style) {
    if entry.starts_with("You: ") || entry.starts_with("> ") || entry.starts_with("You (interject") {
        (
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
            Style::default().fg(Color::Rgb(0xb0, 0xe0, 0xff)),
        )
    } else if entry.starts_with("Agent: ") || entry.starts_with("Agent (partial): ") {
        (
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
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
        (
            Style::default().fg(Color::Yellow),
            Style::default().fg(Color::Yellow),
        )
    } else if entry.starts_with("Raven Hotel - Loaded session")
        || entry.starts_with("Use ↑/↓")
    {
        // Subtle instructions and initial session banners — keep them visually
        // separate from actual conversation turns.
        (
            Style::default().fg(Color::DarkGray),
            Style::default().fg(Color::DarkGray),
        )
    } else if entry.contains("enter plan mode") || entry.contains("Do you want to enter plan mode") || entry.contains("Would you like to enter plan mode") {
        // Plan mode entry confirmation question — highlighted in orange (same as plan pane) to get attention.
        (
            Style::default().fg(PLAN_ORANGE).add_modifier(Modifier::BOLD),
            Style::default().fg(Color::Rgb(0xff, 0xd0, 0x80)),
        )
    } else {
        (
            Style::default().fg(Color::Rgb(0x88, 0x88, 0xaa)),
            Style::default().fg(Color::Rgb(0x88, 0x88, 0xaa)),
        )
    }
}

fn trace_line_style(line: &str) -> Style {
    // 1 🔧 / 🧠 icon at start of tool/thought block; continuations indented (no repeat icon).
    if line.starts_with("🔧") {
        Style::default().fg(Color::Rgb(0xff, 0xc0, 0x60))
    } else if line.starts_with("🧠") {
        Style::default()
            .fg(Color::Rgb(0xa0, 0x80, 0xc0))
            .add_modifier(Modifier::ITALIC)
    } else if line.starts_with("   ↳") {
        Style::default().fg(Color::Rgb(0x80, 0xb0, 0x80))
    } else if line.starts_with("   ⭐⭐") {
        Style::default().fg(Color::Rgb(0xcc, 0xcc, 0xdd))
    } else if line.starts_with("   ") {
        // Continuation of a brain thought block (only the first line of the block has the 🧠 icon)
        Style::default()
            .fg(Color::Rgb(0xa0, 0x80, 0xc0))
            .add_modifier(Modifier::ITALIC)
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

/// Horizontal space reserved inside a scrollable pane: scrollbar(1) + borders(2) + padding(2).
const PANE_INNER_WIDTH_RESERVE: u16 = 5;
const PANE_BORDER_HEIGHT: u16 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PaneScrollMetrics {
    line_count: u16,
    content_height: u16,
    max_scroll: u16,
}

/// Compute wrapped-line scroll limits for a scrollable pane.
///
/// The Paragraph widget uses `Wrap { trim: false }`, so logical lines wider than
/// the inner width occupy multiple visual rows. `max_scroll` is derived from the
/// visible content height (inside borders, minus an optional title row).
fn pane_scroll_metrics(area: Rect, text: &Text, has_title: bool) -> PaneScrollMetrics {
    let title_height = u16::from(has_title);
    let content_height = area.height.saturating_sub(PANE_BORDER_HEIGHT + title_height);
    let inner_width = area.width.saturating_sub(PANE_INNER_WIDTH_RESERVE).max(1) as usize;
    let line_count = count_visual_lines(text, inner_width);
    let max_scroll = line_count.saturating_sub(content_height);
    PaneScrollMetrics {
        line_count,
        content_height,
        max_scroll,
    }
}

fn count_visual_lines(text: &Text, inner_width: usize) -> u16 {
    let inner_width = inner_width.max(1);
    let mut visual_lines: u16 = 0;
    for line in text.lines.iter() {
        let line_width: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
        if line_width == 0 {
            visual_lines += 1; // empty line still takes 1 row
        } else {
            visual_lines += line_width.div_ceil(inner_width) as u16;
        }
    }
    visual_lines
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
    let has_title = !title.trim().is_empty();
    let PaneScrollMetrics {
        line_count,
        content_height,
        max_scroll,
    } = pane_scroll_metrics(area, text, has_title);
    *last_line_count = line_count;
    if follow_output {
        *scroll = max_scroll;
    } else {
        *scroll = (*scroll).min(max_scroll);
    }

    let focus_style = if focused {
        if scroll_flash_timer > 0 {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        }
    } else {
        Style::default().fg(Color::Rgb(0x55, 0x55, 0x66))
    };

    let block = if title.trim().is_empty() {
        Paragraph::new(text.clone())
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(focus_style)
                    .padding(ratatui::widgets::Padding::new(1, 1, 0, 0))
                    .style(Style::default().bg(Color::Black)),
            )
            .wrap(Wrap { trim: false })
            .scroll((*scroll, 0))
    } else {
        let title_line = if let Some(sub) = subtitle {
            Line::from(vec![
                Span::styled(
                    title,
                    Style::default()
                        .fg(title_color)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(sub, Style::default().fg(Color::DarkGray)),
            ])
        } else {
            Line::from(Span::styled(
                title,
                Style::default()
                    .fg(title_color)
                    .add_modifier(Modifier::BOLD),
            ))
        };
        Paragraph::new(text.clone())
            .block(
                Block::default()
                    .title(title_line)
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(focus_style)
                    .padding(ratatui::widgets::Padding::new(1, 1, 0, 0))
                    .style(Style::default().bg(Color::Black)),
            )
            .wrap(Wrap { trim: false })
            .scroll((*scroll, 0))
    };

    f.render_widget(block, area);

    if line_count > content_height {
        use ratatui::widgets::{Scrollbar, ScrollbarOrientation, ScrollbarState};
        let mut sb_state = ScrollbarState::new(line_count as usize).position((*scroll) as usize);
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None);
        f.render_stateful_widget(scrollbar, area, &mut sb_state);
    }
}

fn truncate_str(s: &str, max_chars: usize) -> String {
    if char_count(s) <= max_chars {
        return s.to_string();
    }
    let truncated: String = s.chars().take(max_chars).collect();
    format!("{}…", truncated)
}

fn char_count(s: &str) -> usize {
    s.chars().count()
}

fn push_chars<'a>(dst: &mut String, src: &'a str, max_chars: usize) -> &'a str {
    if max_chars == 0 {
        return src;
    }
    let mut end = 0usize;
    for (n, (i, ch)) in src.char_indices().enumerate() {
        if n >= max_chars {
            break;
        }
        end = i + ch.len_utf8();
    }
    if end == 0 && !src.is_empty() {
        let ch = src.chars().next().unwrap();
        end = ch.len_utf8();
    }
    dst.push_str(&src[..end]);
    &src[end..]
}

/// Wrap approval detail text to fit inside the popup without spilling past its bounds.
fn wrap_approval_lines(s: &str, width: usize, max_lines: usize) -> Vec<String> {
    let width = width.max(10);
    let max_lines = max_lines.max(1);
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut truncated = false;

    'words: for segment in s.split_whitespace() {
        let mut seg = segment;
        while !seg.is_empty() {
            if lines.len() >= max_lines {
                truncated = true;
                break 'words;
            }

            let avail =
                width.saturating_sub(char_count(&current) + usize::from(!current.is_empty()));
            if avail == 0 {
                lines.push(std::mem::take(&mut current));
                continue;
            }

            if char_count(seg) <= avail {
                if !current.is_empty() {
                    current.push(' ');
                }
                current.push_str(seg);
                seg = "";
            } else if current.is_empty() {
                let mut chunk = String::new();
                seg = push_chars(&mut chunk, seg, avail);
                current = chunk;
                if !seg.is_empty() {
                    lines.push(std::mem::take(&mut current));
                }
            } else {
                lines.push(std::mem::take(&mut current));
            }
        }
    }

    if !current.is_empty() && lines.len() < max_lines {
        lines.push(current);
    } else if !current.is_empty() {
        truncated = true;
    }

    if lines.is_empty() {
        lines.push(truncate_str(s, width));
    } else if truncated {
        let last = lines.last_mut().unwrap();
        if !last.ends_with('…') {
            let mut shortened: String = last.chars().take(width.saturating_sub(1)).collect();
            shortened.push('…');
            *last = shortened;
        }
    }

    lines
}

#[cfg(test)]
mod scroll_metrics_tests {
    use super::{count_visual_lines, pane_scroll_metrics};
    use ratatui::{
        layout::Rect,
        text::{Line, Text},
    };

    fn text_from_lines(lines: &[&str]) -> Text<'static> {
        Text::from(
            lines
                .iter()
                .map(|s| Line::from(s.to_string()))
                .collect::<Vec<_>>(),
        )
    }

    #[test]
    fn empty_text_single_visual_line() {
        let text = Text::default();
        assert_eq!(count_visual_lines(&text, 10), 0);
    }

    #[test]
    fn short_lines_one_row_each() {
        let text = text_from_lines(&["hello", "world"]);
        assert_eq!(count_visual_lines(&text, 20), 2);
    }

    #[test]
    fn long_line_wraps_to_multiple_rows() {
        let text = text_from_lines(&["abcdefghijklmnop"]); // 16 chars
        assert_eq!(count_visual_lines(&text, 10), 2);
        assert_eq!(count_visual_lines(&text, 5), 4);
    }

    #[test]
    fn empty_logical_line_counts_as_one_row() {
        let text = text_from_lines(&[""]);
        assert_eq!(count_visual_lines(&text, 10), 1);
    }

    #[test]
    fn titled_pane_reduces_content_height() {
        let area = Rect::new(0, 0, 30, 12);
        let text = text_from_lines(&["line"]);
        let untitled = pane_scroll_metrics(area, &text, false);
        let titled = pane_scroll_metrics(area, &text, true);
        assert_eq!(untitled.content_height, 10);
        assert_eq!(titled.content_height, 9);
    }

    #[test]
    fn inner_width_reserves_scrollbar_and_padding() {
        let area = Rect::new(0, 0, 20, 10);
        // inner width = 20 - 5 = 15
        let text = text_from_lines(&["x".repeat(30).as_str()]);
        assert_eq!(count_visual_lines(&text, 15), 2);
        let metrics = pane_scroll_metrics(area, &text, false);
        assert_eq!(metrics.line_count, 2);
        assert_eq!(metrics.content_height, 8);
        assert_eq!(metrics.max_scroll, 0);
    }

    #[test]
    fn max_scroll_allows_reaching_bottom() {
        let area = Rect::new(0, 0, 20, 8);
        let lines: Vec<String> = (0..20).map(|i| format!("line {i}")).collect();
        let text = Text::from(
            lines
                .iter()
                .map(|s| Line::from(s.as_str()))
                .collect::<Vec<_>>(),
        );
        let metrics = pane_scroll_metrics(area, &text, true);
        // height 8 - borders 2 - title 1 = 5 visible rows; 20 lines -> max_scroll 15
        assert_eq!(metrics.content_height, 5);
        assert_eq!(metrics.line_count, 20);
        assert_eq!(metrics.max_scroll, 15);
    }

    #[test]
    fn metrics_are_stable_across_calls() {
        let area = Rect::new(0, 0, 25, 10);
        let text = text_from_lines(&["alpha", "beta beta beta beta"]);
        let first = pane_scroll_metrics(area, &text, true);
        let second = pane_scroll_metrics(area, &text, true);
        assert_eq!(first, second);
        assert!(first.max_scroll <= first.line_count);
    }
}

#[cfg(test)]
mod approval_popup_tests {
    use super::{char_count, wrap_approval_lines};

    #[test]
    fn wrap_short_stays_single_line() {
        let lines = wrap_approval_lines("cargo test", 40, 4);
        assert_eq!(lines, vec!["cargo test"]);
    }

    #[test]
    fn wrap_breaks_long_unbroken_token() {
        let lines = wrap_approval_lines("a".repeat(30).as_str(), 12, 4);
        assert!(lines.len() > 1);
        assert!(lines.iter().all(|l| char_count(l) <= 12));
    }

    #[test]
    fn wrap_caps_line_count_with_ellipsis() {
        let text = (0..20)
            .map(|i| format!("word{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        let lines = wrap_approval_lines(&text, 16, 3);
        assert_eq!(lines.len(), 3);
        assert!(lines.last().unwrap().ends_with('…'));
    }
}

// ─── Splash / multi-desktop ───────────────────────────────────────────────────

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub struct SplashData<'a> {
    pub raven_art: &'a str,
    pub base_url: &'a str,
    pub model: &'a str,
    pub workspace: &'a str,
    /// Which side of the first screen is highlighted (default Magenta).
    pub splash_focus: crate::app_state::SplashFocus,
}

pub struct PickerDrawData<'a> {
    // Tree combines workspaces + indented sessions under them (sessions pane removed)
    pub picker_items: &'a [crate::app_state::PickerItem],
    pub selected_item: usize,
    pub focus: crate::app_state::PickerFocus,
    pub summary: &'a str,
    pub summary_scroll: usize,
    pub wiki_links: &'a [WikiLink],
    pub active_link_idx: usize,
    pub summary_action: crate::app_state::SummaryAction,
    /// When true, `summary` is wiki markdown (render richly); otherwise plain session meta.
    pub summary_is_markdown: bool,
    /// When in the 3-pane overview (picker | nav | content), which column has focus.
    pub view_focus: crate::app_state::ViewFocus,
    // Browser nav for Screen 2 (stable Harness + Wiki tree)
    pub browser_nav_items: &'a [WikiNavItem],
    pub browser_selected_nav: usize,
    pub browser_wiki_content: &'a str,
    pub browser_wiki_scroll: usize,
}

pub struct WorkspaceDrawData<'a> {
    pub left_committed: &'a [String],
    pub current_response: &'a str,
    pub trace_lines: &'a [String],
    pub current_thinking: &'a str,
    pub left_scroll: u16,
    pub right_scroll: u16,
    pub left_focused: bool,
    pub right_focused: bool,
    pub scroll_flash_timer: u8,
    pub left_highlight: Option<usize>,
    pub right_highlight: Option<usize>,
    // Plan mode overlay pane (shown when run mode == "plan" or plan.active)
    pub plan: Option<&'a PlanState>,
}

pub fn draw_content_desktop(
    f: &mut Frame,
    content_area: Rect,
    desktop: &DesktopState,
    workspace: &WorkspaceDrawData<'_>,
    splash: &SplashData<'_>,
    picker: &PickerDrawData<'_>,
    _wiki_viewer: &crate::app_state::WikiViewerState,
    last_left_area: &mut Rect,
    last_right_area: &mut Rect,
    last_left_line_count: &mut u16,
    last_right_line_count: &mut u16,
    left_scroll: &mut u16,
    right_scroll: &mut u16,
    left_follow_output: bool,
    right_follow_output: bool,
) {
    if matches!(desktop.active, ActiveDesktop::Picker) {
        draw_picker(f, content_area, picker);
        *last_left_area = Rect::default();
        *last_right_area = Rect::default();
        return;
    }

    if desktop.is_animating() {
        let progress = desktop.slide_progress();
        let width = content_area.width as i32;
        let offset = (progress * width as f32).round() as i32;

        let (splash_rel_x, workspace_rel_x) = match desktop.slide_direction() {
            Some(SlideDirection::ToSplash) => (-width + offset, offset),
            Some(SlideDirection::ToWorkspace) => (-offset, width - offset),
            None => (0, 0),
        };

        let mut splash_buf = Buffer::empty(content_area);
        render_splash_to_buffer(&mut splash_buf, content_area, splash);

        let mut workspace_buf = Buffer::empty(content_area);
        render_workspace_to_buffer(&mut workspace_buf, content_area, workspace);

        f.render_widget(
            BlitWidget {
                src: splash_buf,
                rel_x: splash_rel_x,
                rel_y: 0,
            },
            content_area,
        );
        f.render_widget(
            BlitWidget {
                src: workspace_buf,
                rel_x: workspace_rel_x,
                rel_y: 0,
            },
            content_area,
        );
        return;
    }

    match desktop.active {
        ActiveDesktop::Splash => {
            // Horizontal split: left = magenta pane with raven + help, right = workspace picker (full height, outside magenta)
            let cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
                .split(content_area);

            let left_area = cols[0];
            let right_area = cols[1];

            let magenta_focused = splash.splash_focus == crate::app_state::SplashFocus::Magenta;
            let picker_focused = !magenta_focused;

            // Magenta pane on left enclosing the ASCII raven and help block.
            // Bright when focused (default), gray when picker is highlighted.
            let magenta_border = if magenta_focused {
                Style::default().fg(Color::Rgb(0xc0, 0x80, 0xff))
            } else {
                Style::default().fg(Color::Rgb(0x55, 0x55, 0x66))
            };
            let outer_block = Block::default()
                .title(Line::from(vec![
                    Span::styled(
                        "  Raven Hotel",
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("  v{VERSION}"),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled("  —  Agent Harness", Style::default().fg(Color::DarkGray)),
                ]))
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(magenta_border)
                .style(Style::default().bg(Color::Black));

            let left_inner = outer_block.inner(left_area);
            f.render_widget(outer_block, left_area);

            // Render raven + help inside the magenta pane (full height of left column)
            let mut left_buf = Buffer::empty(left_inner);
            render_splash_content(&mut left_buf, left_inner, splash);
            f.render_widget(
                BlitWidget {
                    src: left_buf,
                    rel_x: 0,
                    rel_y: 0,
                },
                left_inner,
            );

            // Workspace picker (tree only) on the right, full vertical space, outside the magenta pane.
            // Gray when magenta focused (default); highlighted (cyan border) after right/tab.
            draw_picker_tree(f, right_area, picker.picker_items, picker.selected_item, picker_focused);

            *last_left_area = Rect::default();
            *last_right_area = Rect::default();
        }
        ActiveDesktop::Overview => {
            // Screen 2: workspace picker | nav | content (wiki or harness conv+status+input)
            // Focus starts on Picker. Right cycles Picker -> Nav -> Content.
            // Up/down affect the focused pane.
            // Right from Content does snap to Screen 3 or 4.
            let cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(30), Constraint::Percentage(30), Constraint::Percentage(40)])
                .split(content_area);

            let picker_area = cols[0];
            let nav_area = cols[1];
            let content_area = cols[2];

            let is_harness = crate::app_state::browser_nav_is_harness(
                picker.browser_nav_items,
                picker.browser_selected_nav,
            );
            let picker_focused = picker.view_focus == crate::app_state::ViewFocus::Picker;
            let nav_focused = picker.view_focus == crate::app_state::ViewFocus::Nav;
            let content_focused = picker.view_focus == crate::app_state::ViewFocus::Content;

            // Picker column
            draw_picker_tree(f, picker_area, picker.picker_items, picker.selected_item, picker_focused);

            // Nav column (Coding Harness at top, Wiki subtree under it; stable, no index.md)
            draw_nav_pane_for_browser(f, nav_area, " Nav ", picker.browser_nav_items, picker.browser_selected_nav, nav_focused);

            // Content column: either wiki content or harness (status + conv + input)
            if is_harness {
                // Allocate space at top of content col for real upper status bar (drawn in event_loop overlay for alignment)
                // then conv, then input space at bottom.
                let vparts = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Length(1), Constraint::Min(6), Constraint::Length(3)])
                    .split(content_area);

                // Conversation content directly (no extra pane frame for embedded Screen 2 view)
                let mut dummy_last = Rect::default();
                let mut dummy_cnt = 0u16;
                let conv_pane_focus = if content_focused { Pane::Left } else { Pane::Right };
                draw_left_pane(
                    f,
                    workspace.left_committed,
                    workspace.current_response,
                    vparts[1],
                    &mut dummy_last,
                    &mut dummy_cnt,
                    false,  // don't auto-follow, allow manual scroll with up/down
                    left_scroll,
                    conv_pane_focus,
                    workspace.scroll_flash_timer,
                    workspace.left_highlight,
                    true,  // draw the Conversation frame with label
                );

                // Bottom space for input box (drawn as overlay in event_loop)
                // (no block here to let real input bar appear)
            } else {
                // Wiki content pane -- use browser content + custom markdown like full wiki
                let wiki_border = if content_focused { Color::Cyan } else { Color::Rgb(0x55,0x55,0x66) };
                let txt = if picker.browser_wiki_content.is_empty() {
                    "(wiki content for selected nav item)".to_string()
                } else {
                    picker.browser_wiki_content.to_string()
                };
                let md_text = render_markdown(&txt);
                let preview = Paragraph::new(md_text)
                    .wrap(Wrap { trim: true })
                    .scroll((picker.browser_wiki_scroll as u16, 0))
                    .block(
                        Block::default()
                            .title(Span::styled(" Wiki ", Style::default().fg(if content_focused { Color::Cyan } else { Color::DarkGray }).add_modifier(Modifier::BOLD)))
                            .borders(Borders::ALL)
                            .border_style(Style::default().fg(wiki_border))
                            .style(Style::default().bg(Color::Black)),
                    );
                f.render_widget(preview, content_area);
            }

            *last_left_area = Rect::default();
            *last_right_area = Rect::default();
        }
        ActiveDesktop::Picker => {
            draw_picker(f, content_area, picker);
            *last_left_area = Rect::default();
            *last_right_area = Rect::default();
        }
        ActiveDesktop::WikiViewer => {
            // Drawing is handled in the caller (event_loop) with full state access for now.
            // Avoid double draw here.
            *last_left_area = Rect::default();
            *last_right_area = Rect::default();
        }
        ActiveDesktop::Workspace => {
            let work_area = if let Some(p) = workspace.plan {
                // Plan pane on top, ~40% height (taller for readability)
                let plan_h = ((content_area.height as f32 * 0.40) as u16).clamp(12, content_area.height.saturating_sub(10));
                let vsplit = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Length(plan_h), Constraint::Min(6)])
                    .split(content_area);
                draw_plan_pane(f, vsplit[0], p);
                vsplit[1]
            } else {
                content_area
            };

            let panes = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(62), Constraint::Percentage(38)])
                .split(work_area);

            let left_focus = if workspace.left_focused {
                Pane::Left
            } else {
                Pane::Right
            };
            let right_focus = if workspace.right_focused {
                Pane::Right
            } else {
                Pane::Left
            };

            draw_left_pane(
                f,
                workspace.left_committed,
                workspace.current_response,
                panes[0],
                last_left_area,
                last_left_line_count,
                left_follow_output,
                left_scroll,
                left_focus,
                workspace.scroll_flash_timer,
                workspace.left_highlight,
                true,
            );
            draw_right_pane(
                f,
                workspace.trace_lines,
                workspace.current_thinking,
                panes[1],
                last_right_area,
                last_right_line_count,
                right_follow_output,
                right_scroll,
                right_focus,
                workspace.scroll_flash_timer,
                workspace.right_highlight,
            );
        }
    }
}

/// Draw only the wiki Nav pane (used by full WikiViewer and by the splash Overview 3-col).
pub fn draw_wiki_nav_pane(f: &mut Frame, area: Rect, viewer: &crate::app_state::WikiViewerState, focused: bool) {
    draw_nav_list(f, area, &format!(" {} ", viewer.current_file), &viewer.nav_items, viewer.selected_nav, focused || viewer.focus == crate::app_state::WikiFocus::Nav);
}

pub fn draw_nav_pane_for_browser(f: &mut Frame, area: Rect, title: &str, items: &[WikiNavItem], selected: usize, focused: bool) {
    draw_nav_list(f, area, title, items, selected, focused);
}

fn draw_nav_list(f: &mut Frame, area: Rect, title: &str, items: &[WikiNavItem], selected: usize, focused: bool) {
    let nav_border = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::Rgb(0x55, 0x55, 0x66))
    };
    let mut nav_text = Text::default();
    nav_text.lines.push(Line::from(Span::styled(title, Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))));
    let sel = selected;
    let nitems = items.len();
    let nav_vis = (area.height as usize).saturating_sub(2).max(1);
    let nav_off = if nitems <= nav_vis || sel < nav_vis / 2 {
        0
    } else if sel + nav_vis / 2 >= nitems {
        nitems.saturating_sub(nav_vis)
    } else {
        sel.saturating_sub(nav_vis / 2)
    };
    for (i, item) in items.iter().enumerate().skip(nav_off).take(nav_vis) {
        let is_sel = i == sel;
        let style = if is_sel {
            Style::default().fg(Color::White).bg(Color::Rgb(0x20, 0x50, 0x80)).add_modifier(Modifier::BOLD)
        } else {
            match item.kind {
                NavItemKind::Back => Style::default().fg(Color::Yellow),
                NavItemKind::Header => Style::default().fg(Color::Rgb(0xcc, 0xcc, 0xdd)),
                NavItemKind::Link => Style::default().fg(Color::Rgb(0x66, 0xcc, 0xee)),
                NavItemKind::Harness => Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            }
        };
        let prefix = if is_sel { "▶ " } else { "  " };
        let shown = truncate_str(&format!("{}{}", prefix, item.label), area.width as usize - 4);
        nav_text.lines.push(Line::from(Span::styled(shown, style)));
    }
    if items.is_empty() {
        nav_text.lines.push(Line::from(Span::styled("  (no nav)", Style::default().fg(Color::DarkGray))));
    }
    let nav_para = Paragraph::new(nav_text)
        .block(Block::default().title(" Nav ").borders(Borders::ALL).border_style(nav_border).style(Style::default().bg(Color::Black)));
    f.render_widget(nav_para, area);
}

pub fn draw_wiki_viewer(f: &mut Frame, area: Rect, viewer: &crate::app_state::WikiViewerState) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
        .split(area);

    let nav_area = cols[0];
    let content_area = cols[1];

    draw_wiki_nav_pane(f, nav_area, viewer, viewer.focus == crate::app_state::WikiFocus::Nav);

    let sel = viewer.selected_nav;

    // Content pane - larger wiki display; highlight active nav target where possible
    let content_border = if viewer.focus == crate::app_state::WikiFocus::Content {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::Rgb(0x55, 0x55, 0x66))
    };
    let mut content_text = Text::default();
    content_text.lines.push(Line::from(Span::styled(
        format!("  {}", viewer.current_file),
        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
    )));
    content_text.lines.push(Line::from(""));

    let md = if viewer.content.is_empty() {
        "(empty wiki file)".to_string()
    } else {
        viewer.content.clone()
    };
    let active_item = viewer.nav_items.get(sel);
    let search = active_item
        .map(|it| nav_highlight_search(it.kind, &it.label))
        .unwrap_or_default();
    let start = viewer.scroll;
    let max = (content_area.height as usize).saturating_sub(4);
    let mut md_text = markdown_viewport(&md, start, max);
    highlight_markdown_viewport(&mut md_text, &search, 0, max);
    for line in md_text.lines {
        content_text.lines.push(line);
    }

    // No Wrap — our renderer controls line layout (tables rely on exact alignment).
    // ratatui clips lines at the widget edge when wrap is disabled.
    let content_para = Paragraph::new(content_text)
        .block(Block::default().title(" Wiki ").borders(Borders::ALL).border_style(content_border).style(Style::default().bg(Color::Black)));
    f.render_widget(content_para, content_area);
}

pub fn draw_picker(f: &mut Frame, area: Rect, data: &PickerDrawData<'_>) {
    // Single tree column (workspaces + indented sessions) + summary.
    // This removes the separate narrow Sessions pane.
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(area);

    draw_picker_tree(f, cols[0], data.picker_items, data.selected_item, data.focus == crate::app_state::PickerFocus::Tree);
    draw_session_summary(
        f,
        cols[1],
        data.summary,
        data.summary_scroll,
        data.focus == crate::app_state::PickerFocus::Summary,
        data.wiki_links,
        data.active_link_idx,
        data.summary_action,
        data.summary_is_markdown,
    );

    // subtle hint line at bottom of area if space
    if area.height > 4 {
        let hint = "↑↓ Tree  ←→ focus  w: wiki  Enter: launch  (right on summary -> full wiki)";
        let hint_area = Rect { y: area.y + area.height - 1, height: 1, ..area };
        f.render_widget(
            Paragraph::new(Span::styled(hint, Style::default().fg(Color::DarkGray))),
            hint_area,
        );
    }
}

fn draw_picker_tree(
    f: &mut Frame,
    area: Rect,
    items: &[crate::app_state::PickerItem],
    selected: usize,
    focused: bool,
) {
    // Combined tree: workspaces at depth 0, their sessions indented (depth 1).
    // Truncation uses the actual inner width of this pane (no hard-coded char limits).
    let mut text = Text::default();

    if items.is_empty() {
        text.lines.push(Line::from(Span::styled("  (no workspaces)", Style::default().fg(Color::DarkGray))));
    } else {
        // Compute usable width from the rendered pane area (borders on each side).
        // We subtract a small margin so text doesn't butt against the right border.
        let inner_width = area.width.saturating_sub(2).max(4) as usize;
        let maxw = inner_width.saturating_sub(2);

        for (i, item) in items.iter().enumerate().take(18) {
            let is_sel = i == selected;
            // Distinguish workspace headers from session rows in the combined tree.
            let prefix = if item.depth == 0 {
                if is_sel { "▶ " } else { "▷ " }
            } else if is_sel {
                "▶ "
            } else {
                "  "
            };
            let indent = "  ".repeat(item.depth);
            let style = if is_sel {
                Style::default().fg(Color::White).bg(Color::Rgb(0x20, 0x50, 0x80)).add_modifier(Modifier::BOLD)
            } else if focused {
                Style::default().fg(Color::White)
            } else {
                Style::default().fg(Color::Rgb(0xaa, 0xaa, 0xaa))
            };
            let label = format!("{}{}{}", prefix, indent, item.label);
            text.lines.push(Line::from(Span::styled(truncate_str(&label, maxw), style)));
        }
    }

    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::Rgb(0x55, 0x55, 0x66))
    };
    let para = Paragraph::new(text)
        .block(
            Block::default()
                .title(" Workspaces / Sessions ")
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(border_style)
                .padding(ratatui::widgets::Padding::new(1, 1, 1, 0))
                .style(Style::default().bg(Color::Black)),
        )
        .wrap(Wrap { trim: false });
    f.render_widget(para, area);
}

// Old column fn kept for now (no longer used by draw_picker)
#[allow(dead_code)]
fn draw_workspace_column(
    f: &mut Frame,
    area: Rect,
    workspaces: &[raven_tui::session::WorkspaceEntry],
    selected: usize,
    focused: bool,
) {
    let mut text = Text::default();
    text.lines.push(Line::from(Span::styled(
        "  Workspaces (recent first)",
        if focused {
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        },
    )));
    text.lines.push(Line::from(""));

    if workspaces.is_empty() {
        text.lines.push(Line::from(Span::styled("  (no previous sessions)", Style::default().fg(Color::DarkGray))));
    } else {
        for (i, ws) in workspaces.iter().enumerate().take(12) {
            let is_sel = i == selected;
            let prefix = if is_sel { "▶ " } else { "  " };
            let style = if is_sel {
                Style::default().fg(Color::White).bg(Color::Rgb(0x20, 0x50, 0x80)).add_modifier(Modifier::BOLD)
            } else if focused {
                Style::default().fg(Color::White)
            } else {
                Style::default().fg(Color::Rgb(0xaa, 0xaa, 0xaa))
            };
            let label = format!(
                "{}{} ({} sess)",
                prefix,
                ws.path.display(),
                ws.session_count
            );
            text.lines.push(Line::from(Span::styled(truncate_str(&label, area.width as usize - 4), style)));
        }
    }

    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::Rgb(0x55, 0x55, 0x66))
    };
    let para = Paragraph::new(text)
        .block(
            Block::default()
                .title(" Workspaces ")
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(border_style)
                .style(Style::default().bg(Color::Black)),
        )
        .wrap(Wrap { trim: false });
    f.render_widget(para, area);
}

#[allow(dead_code)]
fn draw_sessions_column(
    f: &mut Frame,
    area: Rect,
    sessions: &[raven_tui::session::SessionMeta],
    selected: usize,
    focused: bool,
) {
    let mut text = Text::default();
    text.lines.push(Line::from(Span::styled(
        "  Sessions for workspace",
        if focused {
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        },
    )));
    text.lines.push(Line::from(""));

    if sessions.is_empty() {
        text.lines.push(Line::from(Span::styled("  (no sessions)", Style::default().fg(Color::DarkGray))));
    } else {
        for (i, s) in sessions.iter().enumerate().take(12) {
            let is_sel = i == selected;
            let prefix = if is_sel { "▶ " } else { "  " };
            let style = if is_sel {
                Style::default().fg(Color::White).bg(Color::Rgb(0x20, 0x50, 0x80)).add_modifier(Modifier::BOLD)
            } else if focused {
                Style::default().fg(Color::White)
            } else {
                Style::default().fg(Color::Rgb(0xaa, 0xaa, 0xaa))
            };
            let short_id = &s.session_id[..s.session_id.len().min(28)];
            let label = format!("{}{}  {}", prefix, short_id, &s.updated_at[..s.updated_at.len().min(16)]);
            text.lines.push(Line::from(Span::styled(truncate_str(&label, area.width as usize - 4), style)));
        }
    }

    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::Rgb(0x55, 0x55, 0x66))
    };
    let para = Paragraph::new(text)
        .block(
            Block::default()
                .title(" Sessions ")
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(border_style)
                .style(Style::default().bg(Color::Black)),
        )
        .wrap(Wrap { trim: false });
    f.render_widget(para, area);
}

#[allow(dead_code)] // kept for future reuse
fn draw_button(f: &mut Frame, area: Rect, label: &str, focused: bool) {
    let (fg, border_fg) = if focused {
        (Color::Cyan, Color::Cyan)
    } else {
        (Color::DarkGray, Color::Rgb(0x44, 0x44, 0x55))
    };
    let style = if focused {
        Style::default().fg(fg).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(fg)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_fg));
    let para = Paragraph::new(label)
        .style(style)
        .block(block)
        .alignment(Alignment::Center);
    f.render_widget(para, area);
}

fn draw_session_summary(
    f: &mut Frame,
    area: Rect,
    summary: &str,
    scroll: usize,
    focused: bool,
    _wiki_links: &[WikiLink],
    _active_link_idx: usize,
    _summary_action: crate::app_state::SummaryAction,
    summary_is_markdown: bool,
) {
    let content_area = area;
    let max_lines = (content_area.height as usize).saturating_sub(4);

    let text = if summary.is_empty() {
        Text::from(Line::from(Span::styled(
            "  (no session)",
            Style::default().fg(Color::DarkGray),
        )))
    } else if summary_is_markdown {
        markdown_viewport(summary, scroll, max_lines)
    } else {
        let base_style = Style::default().fg(Color::Rgb(0xcc, 0xcc, 0xdd));
        let mut plain = Text::default();
        for line in summary.lines().skip(scroll).take(max_lines) {
            plain
                .lines
                .push(Line::from(Span::styled(line.to_string(), base_style)));
        }
        plain
    };

    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::Rgb(0x55, 0x55, 0x66))
    };
    let para = Paragraph::new(text)
        .block(
            Block::default()
                .title(" Session Summary ")
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(border_style)
                .style(Style::default().bg(Color::Black)),
        );
    f.render_widget(para, content_area);
}

fn render_splash_to_buffer(buf: &mut Buffer, area: Rect, data: &SplashData<'_>) {
    let block = Block::default()
        .title(Line::from(vec![
            Span::styled(
                "  Raven Hotel",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("  v{VERSION}"),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled("  —  Agent Harness", Style::default().fg(Color::DarkGray)),
        ]))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Rgb(0xc0, 0x80, 0xff)))
        .style(Style::default().bg(Color::Black));

    let inner = block.inner(area);
    block.render(area, buf);
    render_splash_content(buf, inner, data);
}

fn render_splash_content(buf: &mut Buffer, area: Rect, data: &SplashData<'_>) {
    if area.width < 8 || area.height < 6 {
        return;
    }

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(42), Constraint::Percentage(58)])
        .split(area);

    let art_style = Style::default().fg(Color::Rgb(0xc0, 0x80, 0xff));
    // Add extra padding above and to the left of the ASCII raven art
    let art_area = cols[0];
    let padded_art = Rect {
        x: art_area.x + 3,           // extra left padding
        y: art_area.y + 2,           // extra top padding
        width: art_area.width.saturating_sub(6),
        height: art_area.height.saturating_sub(3),
    };
    Paragraph::new(data.raven_art)
        .style(art_style)
        .wrap(Wrap { trim: false })
        .render(padded_art, buf);

    let hint = Style::default().fg(Color::DarkGray);
    let accent = Style::default().fg(Color::Rgb(0xa0, 0xd0, 0xff));
    let key = Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD);

    let help = Text::from(vec![
        Line::from(Span::styled(
            "Local-first agentic coding harness",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("→", key),
            Span::styled("  Right arrow", accent),
            Span::styled("  workspace/session picker", hint),
        ]),
        Line::from(vec![
            Span::styled("←", key),
            Span::styled("  Left arrow", accent),
            Span::styled("  back from picker / panes", hint),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "Navigation",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(vec![
            Span::styled("Tab", key),
            Span::styled("  cycle focus (Conv → Trace → Input)", hint),
        ]),
        Line::from(vec![
            Span::styled("↑↓", key),
            Span::styled("  scroll pane  •  ", hint),
            Span::styled("Ctrl+↑↓", key),
            Span::styled("  input history", hint),
        ]),
        Line::from(vec![
            Span::styled("Ctrl+F", key),
            Span::styled("  search  •  ", hint),
            Span::styled("/help", key),
            Span::styled("  slash commands", hint),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "Copy / paste",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(vec![
            Span::styled("Shift+drag", key),
            Span::styled("  terminal selection  •  ", hint),
            Span::styled("Ctrl+Insert", key),
            Span::styled("  copy", hint),
        ]),
        Line::from(vec![
            Span::styled("Shift+Insert", key),
            Span::styled("  paste  •  ", hint),
            Span::styled("Ctrl+V", key),
            Span::styled("  paste (when clipboard works)", hint),
        ]),
        Line::from(Span::styled(
            "SSH/screen vary by emulator — bracketed paste and Ctrl+V are tried automatically",
            hint,
        )),
        Line::from(""),
        Line::from(Span::styled(
            "Session",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(vec![
            Span::styled("endpoint: ", hint),
            Span::styled(data.base_url, accent),
        ]),
        Line::from(vec![
            Span::styled("model:    ", hint),
            Span::styled(data.model, accent),
        ]),
        Line::from(vec![
            Span::styled("workspace:", hint),
            Span::styled(truncate_str(data.workspace, 48), accent),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "Goals, repo cache, and summaries persist under ~/.raven-hotel/",
            hint,
        )),
    ]);

    Paragraph::new(help)
        .wrap(Wrap { trim: true })
        .render(cols[1], buf);
}

fn render_workspace_to_buffer(buf: &mut Buffer, content_area: Rect, data: &WorkspaceDrawData<'_>) {
    draw_workspace_panes_buf(
        buf,
        content_area,
        data,
        &mut Rect::default(),
        &mut Rect::default(),
        &mut 0,
        &mut 0,
    );
}

fn draw_workspace_panes_buf(
    buf: &mut Buffer,
    content_area: Rect,
    data: &WorkspaceDrawData<'_>,
    last_left_area: &mut Rect,
    last_right_area: &mut Rect,
    last_left_line_count: &mut u16,
    last_right_line_count: &mut u16,
) {
    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(62), Constraint::Percentage(38)])
        .split(content_area);

    let mut left_scroll = data.left_scroll;
    let mut right_scroll = data.right_scroll;

    let left_focus = if data.left_focused {
        Pane::Left
    } else {
        Pane::Right
    };
    let right_focus = if data.right_focused {
        Pane::Right
    } else {
        Pane::Left
    };

    render_left_pane_to_buffer(
        buf,
        data.left_committed,
        data.current_response,
        panes[0],
        last_left_area,
        last_left_line_count,
        &mut left_scroll,
        left_focus,
        data.scroll_flash_timer,
        data.left_highlight,
    );

    render_right_pane_to_buffer(
        buf,
        data.trace_lines,
        data.current_thinking,
        panes[1],
        last_right_area,
        last_right_line_count,
        &mut right_scroll,
        right_focus,
        data.scroll_flash_timer,
        data.right_highlight,
    );
}

/// Blit a pre-rendered buffer into the target, offset relative to `area` origin.
struct BlitWidget {
    src: Buffer,
    rel_x: i32,
    rel_y: i32,
}

impl Widget for BlitWidget {
    fn render(self, area: Rect, buf: &mut Buffer) {
        blit_to_buffer(
            buf,
            &self.src,
            area.x as i32 + self.rel_x,
            area.y as i32 + self.rel_y,
            area,
        );
    }
}

fn blit_to_buffer(dst: &mut Buffer, src: &Buffer, dest_x: i32, dest_y: i32, clip: Rect) {
    let src_area = src.area;
    for y in 0..src_area.height {
        for x in 0..src_area.width {
            let screen_x = dest_x + x as i32;
            let screen_y = dest_y + y as i32;
            if screen_x < clip.x as i32
                || screen_y < clip.y as i32
                || screen_x >= (clip.x + clip.width) as i32
                || screen_y >= (clip.y + clip.height) as i32
            {
                continue;
            }
            if let Some(cell) = src.cell((x, y)) {
                if let Some(dst_cell) = dst.cell_mut((screen_x as u16, screen_y as u16)) {
                    *dst_cell = cell.clone();
                }
            }
        }
    }
}

fn render_left_pane_to_buffer(
    buf: &mut Buffer,
    left_committed: &[String],
    current_response: &str,
    left_area: Rect,
    last_left_area: &mut Rect,
    last_left_line_count: &mut u16,
    left_scroll: &mut u16,
    focused_pane: Pane,
    scroll_flash_timer: u8,
    highlight_line: Option<usize>,
) {
    *last_left_area = left_area;
    let mut left_text = build_left_text(left_committed, current_response, highlight_line);
    render_scrollable_pane_buf(
        buf,
        left_area,
        &mut left_text,
        last_left_line_count,
        left_scroll,
        focused_pane == Pane::Left,
        scroll_flash_timer,
        "  Conversation",
        Color::Cyan,
        Some(format!("  ({} msgs)", left_committed.len())),
    );
}

fn render_right_pane_to_buffer(
    buf: &mut Buffer,
    trace_lines: &[String],
    current_thinking: &str,
    right_area: Rect,
    last_right_area: &mut Rect,
    last_right_line_count: &mut u16,
    right_scroll: &mut u16,
    focused_pane: Pane,
    scroll_flash_timer: u8,
    highlight_line: Option<usize>,
) {
    *last_right_area = right_area;
    let mut right_text = build_right_text(trace_lines, current_thinking, highlight_line);
    render_scrollable_pane_buf(
        buf,
        right_area,
        &mut right_text,
        last_right_line_count,
        right_scroll,
        focused_pane == Pane::Right,
        scroll_flash_timer,
        "  Trace",
        Color::Rgb(0xd0, 0xa0, 0xff),
        None,
    );
}

fn build_left_text(
    left_committed: &[String],
    current_response: &str,
    highlight_line: Option<usize>,
) -> Text<'static> {
    let mut left_text = Text::default();
    let mut line_idx = 0usize;
    let mut consecutive_blanks = 0usize;

    for (i, entry) in left_committed.iter().enumerate() {
        if is_plan_entry_confirm(entry) {
            push_attention_entry(&mut left_text, entry, highlight_line, &mut line_idx, PLAN_ORANGE);
        } else if entry.starts_with("You: ") || entry.starts_with("> ") || entry.starts_with("You (interject") {
            let (prefix_style, body_style) = conversation_entry_styles(entry);
            let lines_iter: Vec<&str> = entry.lines().collect();
            for (li, line) in lines_iter.iter().enumerate() {
                let is_blank = line.trim().is_empty();
                if is_blank {
                    consecutive_blanks += 1;
                    if consecutive_blanks > 2 {
                        continue;
                    }
                } else {
                    consecutive_blanks = 0;
                }
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
        } else {
            let md = render_markdown(entry);
            for mut line in md.lines {
                if Some(line_idx) == highlight_line {
                    for sp in &mut line.spans {
                        sp.style = sp.style
                            .fg(Color::Black)
                            .bg(Color::Magenta)
                            .add_modifier(Modifier::BOLD);
                    }
                }
                let is_blank = line.spans.is_empty() || line.spans.iter().all(|s| s.content.trim().is_empty());
                if is_blank {
                    consecutive_blanks += 1;
                    if consecutive_blanks <= 2 {
                        left_text.lines.push(line);
                    }
                } else {
                    consecutive_blanks = 0;
                    left_text.lines.push(line);
                }
                line_idx += 1;
            }
        }
        if i < left_committed.len() - 1 && consecutive_blanks < 2 {
            left_text.lines.push(Line::from(""));
            line_idx += 1;
            consecutive_blanks += 1;
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
            let is_blank = line.trim().is_empty();
            if is_blank {
                consecutive_blanks += 1;
                if consecutive_blanks > 2 {
                    continue;
                }
            } else {
                consecutive_blanks = 0;
            }
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

    left_text
}

fn build_right_text(
    trace_lines: &[String],
    current_thinking: &str,
    highlight_line: Option<usize>,
) -> Text<'static> {
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
                .fg(Color::LightCyan)
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

    right_text
}

fn render_scrollable_pane_buf(
    buf: &mut Buffer,
    area: Rect,
    text: &mut Text,
    last_line_count: &mut u16,
    scroll: &mut u16,
    focused: bool,
    scroll_flash_timer: u8,
    title: &str,
    title_color: Color,
    subtitle: Option<String>,
) {
    let has_title = !title.trim().is_empty();
    let PaneScrollMetrics {
        line_count,
        max_scroll,
        ..
    } = pane_scroll_metrics(area, text, has_title);
    *last_line_count = line_count;
    *scroll = (*scroll).min(max_scroll);

    let focus_style = if focused {
        if scroll_flash_timer > 0 {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        }
    } else {
        Style::default().fg(Color::Rgb(0x55, 0x55, 0x66))
    };

    let title_line = if let Some(sub) = subtitle {
        Line::from(vec![
            Span::styled(
                title,
                Style::default()
                    .fg(title_color)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(sub, Style::default().fg(Color::DarkGray)),
        ])
    } else {
        Line::from(Span::styled(
            title,
            Style::default()
                .fg(title_color)
                .add_modifier(Modifier::BOLD),
        ))
    };

    Paragraph::new(text.clone())
        .block(
            Block::default()
                .title(title_line)
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(focus_style)
                .padding(ratatui::widgets::Padding::new(1, 1, 0, 0))
                .style(Style::default().bg(Color::Black)),
        )
        .wrap(Wrap { trim: false })
        .scroll((*scroll, 0))
        .render(area, buf);
}
