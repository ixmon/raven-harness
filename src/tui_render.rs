//! Extracted TUI rendering helpers (glm.md refactor).

#![allow(clippy::too_many_arguments)]

use crate::desktop::{ActiveDesktop, DesktopState, SlideDirection};
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

// tui-markdown: converts markdown → ratatui Text with styled headings, code, links, lists.
// ratatui 0.29 + tui-markdown 0.3.8 share ratatui-core 0.1.2 so types are identical.
// We only need to convert borrowed Cow::Borrowed spans to owned for 'static lifetime.

/// Custom style sheet for Raven's dark TUI theme.
/// The default tui-markdown styles (white-on-cyan H1, blue links) look off on our
/// dark background (#1a1a22). This gives us headings and links that fit.
#[derive(Clone, Copy, Debug, Default)]
struct RavenStyleSheet;

impl tui_markdown::StyleSheet for RavenStyleSheet {
    fn heading(&self, level: u8) -> ratatui_core::style::Style {
        match level {
            1 => ratatui_core::style::Style::new().light_cyan().bold().underlined(),
            2 => ratatui_core::style::Style::new().cyan().bold(),
            3 => ratatui_core::style::Style::new().cyan().italic(),
            _ => ratatui_core::style::Style::new().light_cyan().italic(),
        }
    }
    fn code(&self) -> ratatui_core::style::Style {
        // Subtle dark bg for code blocks/inline code
        ratatui_core::style::Style::new().fg(ratatui_core::style::Color::Rgb(0xc0, 0xc0, 0xd0)).bg(ratatui_core::style::Color::Rgb(0x28, 0x28, 0x35))
    }
    fn link(&self) -> ratatui_core::style::Style {
        ratatui_core::style::Style::new().cyan().underlined()
    }
    fn blockquote(&self) -> ratatui_core::style::Style {
        ratatui_core::style::Style::new().fg(ratatui_core::style::Color::Rgb(0x80, 0xb0, 0x80)).italic()
    }
    fn heading_meta(&self) -> ratatui_core::style::Style {
        ratatui_core::style::Style::new().dim()
    }
    fn metadata_block(&self) -> ratatui_core::style::Style {
        ratatui_core::style::Style::new().fg(ratatui_core::style::Color::Rgb(0xaa, 0xaa, 0x60))
    }
}

fn wiki_render_markdown(md: &str) -> Text<'static> {
    // Pre-process: convert markdown tables to box-drawn text (tui_markdown doesn't support tables)
    let processed = preprocess_tables(md);
    let opts = tui_markdown::Options::new(RavenStyleSheet);
    let text = tui_markdown::from_str_with_options(&processed, &opts);
    // Convert ratatui_core types to ratatui types (structurally identical but separate types)
    let lines: Vec<Line<'static>> = text
        .lines
        .into_iter()
        .map(|line| {
            let spans: Vec<Span<'static>> = line
                .spans
                .into_iter()
                .map(|span| {
                    let s = convert_core_style(span.style);
                    let content = span.content.to_string();
                    // Strip "# " prefixes from heading spans — the style already indicates level
                    let cleaned = strip_heading_hashes(&content);
                    Span::styled(cleaned, s)
                })
                .collect();
            Line::from(spans)
        })
        .collect();
    Text::from(lines)
}

/// Strip leading `#` markers from heading text.
/// tui_markdown outputs headings as "# " + text; the style already differentiates levels.
fn strip_heading_hashes(s: &str) -> String {
    let trimmed = s.trim_start();
    if trimmed.starts_with('#') {
        // Only strip if it's purely hash+space prefix (e.g. "## " or "### ")
        let after_hashes = trimmed.trim_start_matches('#');
        if after_hashes.is_empty() || after_hashes.starts_with(' ') {
            return after_hashes.trim_start().to_string();
        }
    }
    s.to_string()
}

/// Pre-process markdown tables into box-drawn text for TUI display.
/// Converts `| A | B |` style tables into aligned columns with unicode borders.
fn preprocess_tables(md: &str) -> String {
    let mut result = String::new();
    let lines: Vec<&str> = md.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        // Detect table: line with pipes, followed by separator line (|---|---|)
        if i + 1 < lines.len() && is_table_row(lines[i]) && is_separator_row(lines[i + 1]) {
            // Collect all table rows
            let mut table_rows: Vec<Vec<String>> = vec![];
            let header = parse_table_row(lines[i]);
            table_rows.push(header);
            i += 1; // skip header
            i += 1; // skip separator
            while i < lines.len() && is_table_row(lines[i]) && !is_separator_row(lines[i]) {
                table_rows.push(parse_table_row(lines[i]));
                i += 1;
            }
            // Render the table with box drawing
            result.push_str(&render_box_table(&table_rows));
            result.push('\n');
        } else {
            result.push_str(lines[i]);
            result.push('\n');
            i += 1;
        }
    }
    result
}

fn is_table_row(line: &str) -> bool {
    let t = line.trim();
    t.starts_with('|') && t.ends_with('|') && t.len() > 2
}

fn is_separator_row(line: &str) -> bool {
    let t = line.trim();
    t.starts_with('|') && t.contains("---")
}

fn parse_table_row(line: &str) -> Vec<String> {
    let t = line.trim().trim_matches('|');
    t.split('|').map(|cell| cell.trim().to_string()).collect()
}

fn render_box_table(rows: &[Vec<String>]) -> String {
    if rows.is_empty() {
        return String::new();
    }
    let ncols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    // Calculate column widths
    let mut widths = vec![0usize; ncols];
    for row in rows {
        for (j, cell) in row.iter().enumerate() {
            if j < ncols {
                widths[j] = widths[j].max(cell.len());
            }
        }
    }
    // Minimum width
    for w in &mut widths {
        *w = (*w).max(3);
    }

    let mut out = String::new();
    // Top border: ┌───┬───┐
    out.push('┌');
    for (j, w) in widths.iter().enumerate() {
        out.push_str(&"─".repeat(*w + 2));
        if j + 1 < ncols { out.push('┬'); }
    }
    out.push_str("┐\n");

    for (i, row) in rows.iter().enumerate() {
        // Data row: │ val │ val │
        out.push('│');
        for (j, w) in widths.iter().enumerate() {
            let cell = row.get(j).map(|s| s.as_str()).unwrap_or("");
            out.push_str(&format!(" {:<width$} │", cell, width = w));
        }
        out.push('\n');

        if i == 0 && rows.len() > 1 {
            // Header separator: ├───┼───┤
            out.push('├');
            for (j, w) in widths.iter().enumerate() {
                out.push_str(&"─".repeat(*w + 2));
                if j + 1 < ncols { out.push('┼'); }
            }
            out.push_str("┤\n");
        }
    }

    // Bottom border: └───┴───┘
    out.push('└');
    for (j, w) in widths.iter().enumerate() {
        out.push_str(&"─".repeat(*w + 2));
        if j + 1 < ncols { out.push('┴'); }
    }
    out.push_str("┘\n");

    out
}

/// Convert ratatui_core::style::Style → ratatui::style::Style.
/// These are structurally identical but different types due to the ratatui / ratatui-core split.
fn convert_core_style(cs: ratatui_core::style::Style) -> Style {
    let mut s = Style::default();
    if let Some(fg) = cs.fg {
        s = s.fg(convert_core_color(fg));
    }
    if let Some(bg) = cs.bg {
        s = s.bg(convert_core_color(bg));
    }
    s = s.add_modifier(convert_core_modifier(cs.add_modifier));
    s = s.remove_modifier(convert_core_modifier(cs.sub_modifier));
    s
}

fn convert_core_color(c: ratatui_core::style::Color) -> Color {
    match c {
        ratatui_core::style::Color::Reset => Color::Reset,
        ratatui_core::style::Color::Black => Color::Black,
        ratatui_core::style::Color::Red => Color::Red,
        ratatui_core::style::Color::Green => Color::Green,
        ratatui_core::style::Color::Yellow => Color::Yellow,
        ratatui_core::style::Color::Blue => Color::Blue,
        ratatui_core::style::Color::Magenta => Color::Magenta,
        ratatui_core::style::Color::Cyan => Color::Cyan,
        ratatui_core::style::Color::Gray => Color::Gray,
        ratatui_core::style::Color::DarkGray => Color::DarkGray,
        ratatui_core::style::Color::LightRed => Color::LightRed,
        ratatui_core::style::Color::LightGreen => Color::LightGreen,
        ratatui_core::style::Color::LightYellow => Color::LightYellow,
        ratatui_core::style::Color::LightBlue => Color::LightBlue,
        ratatui_core::style::Color::LightMagenta => Color::LightMagenta,
        ratatui_core::style::Color::LightCyan => Color::LightCyan,
        ratatui_core::style::Color::White => Color::White,
        ratatui_core::style::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
        ratatui_core::style::Color::Indexed(i) => Color::Indexed(i),
    }
}

fn convert_core_modifier(m: ratatui_core::style::Modifier) -> Modifier {
    // Modifier is a bitflag; the bits are identical between ratatui and ratatui_core
    Modifier::from_bits_truncate(m.bits())
}



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
            .style(Style::default().bg(Color::Rgb(0x22, 0x22, 0x33))),
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
            .style(Style::default().bg(Color::Rgb(0x22, 0x22, 0x33))),
    );
    f.render_widget(Clear, menu_area);
    f.render_widget(menu_block, menu_area);
}

pub fn draw_approval_popup(f: &mut Frame, desc: &str, screen: Rect, input_area: Rect) {
    let modal_w = screen.width.saturating_sub(4).clamp(44, 64);
    // Inner text width: borders (2) + default paragraph padding (2).
    let inner_w = modal_w.saturating_sub(4) as usize;
    let available_h = input_area.y.saturating_sub(screen.y + 1).max(7);
    // Header + detail + spacer + Y/N + top/bottom borders.
    let max_desc_lines = ((available_h as usize).saturating_sub(5)).clamp(1, 4);
    let (kind, detail) = desc.split_once(": ").unwrap_or(("action", desc));
    let detail_lines = wrap_approval_lines(detail, inner_w, max_desc_lines);

    let body_lines = 1 + detail_lines.len() + 1 + 1;
    let modal_h = (body_lines as u16 + 2).clamp(7, available_h);

    let modal_x = screen.x + (screen.width.saturating_sub(modal_w)) / 2;
    let modal_y = input_area.y.saturating_sub(modal_h + 1).max(screen.y + 1);
    let modal_area = Rect::new(modal_x, modal_y, modal_w, modal_h);

    let detail_style = Style::default().fg(Color::Rgb(0xaa, 0xaa, 0xaa));
    let mut popup_lines = vec![Line::from(vec![
        Span::styled(
            kind,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            " — sandbox approval needed",
            Style::default().fg(Color::DarkGray),
        ),
    ])];
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
                " Action Approval ",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(Color::Yellow))
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
    agent_mode_menu_active: bool,
    agent_modes: &[&str],
    selected_agent_mode_idx: usize,
) {
    if let Some(desc) = pending_approval {
        draw_approval_popup(f, desc, screen, input_area);
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
    if entry.starts_with("You: ") {
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
    let max_scroll = line_count.saturating_sub(content_height);
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
        let mut sb_state = ScrollbarState::new(line_count as usize).position((*scroll) as usize);
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
}

pub struct PickerDrawData<'a> {
    pub workspaces: &'a [raven_tui::session::WorkspaceEntry],
    pub selected_workspace: usize,
    pub sessions: &'a [raven_tui::session::SessionMeta],
    pub selected_session: usize,
    pub focus: crate::app_state::PickerFocus,
    pub summary: &'a str,
    pub summary_scroll: usize,
    pub wiki_links: &'a [crate::app_state::WikiLink],
    pub active_link_idx: usize,
    pub summary_action: crate::app_state::SummaryAction,
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
}

/// Draw splash or workspace, or animate a horizontal slide between them.
/// When desktop is Picker, draws the two-column session/workspace chooser.
pub fn draw_content_desktop(
    f: &mut Frame,
    content_area: Rect,
    desktop: &DesktopState,
    workspace: &WorkspaceDrawData<'_>,
    splash: &SplashData<'_>,
    picker: &PickerDrawData<'_>,
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
            draw_splash(f, content_area, splash);
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
            let panes = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(62), Constraint::Percentage(38)])
                .split(content_area);

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

pub fn draw_wiki_viewer(f: &mut Frame, area: Rect, viewer: &crate::app_state::WikiViewerState) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
        .split(area);

    let nav_area = cols[0];
    let content_area = cols[1];

    // Nav pane with navigational elements (links + headings + files)
    let nav_border = if viewer.focus == crate::app_state::WikiFocus::Nav {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::Rgb(0x55, 0x55, 0x66))
    };
    let mut nav_text = Text::default();
    let title_label = format!(" {} ", viewer.current_file);
    nav_text.lines.push(Line::from(Span::styled(title_label, Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))));
    let sel = viewer.selected_nav;
    let nitems = viewer.nav_items.len();
    let nav_vis = (nav_area.height as usize).saturating_sub(2).max(1);
    // auto scroll the nav view so the selected element is visible (centered-ish)
    let nav_off = if nitems <= nav_vis || sel < nav_vis / 2 {
        0
    } else if sel + nav_vis / 2 >= nitems {
        nitems.saturating_sub(nav_vis)
    } else {
        sel.saturating_sub(nav_vis / 2)
    };
    for (i, item) in viewer.nav_items.iter().enumerate().skip(nav_off).take(nav_vis) {
        let is_sel = i == sel;
        let style = if is_sel {
            Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD)
        } else {
            match item.kind {
                crate::app_state::NavItemKind::Back => Style::default().fg(Color::Yellow),
                crate::app_state::NavItemKind::Header => Style::default().fg(Color::Rgb(0xcc, 0xcc, 0xdd)),
                crate::app_state::NavItemKind::Link => Style::default().fg(Color::Rgb(0x66, 0xcc, 0xee)),
            }
        };
        let prefix = if is_sel { "▶ " } else { "  " };
        let shown = truncate_str(&format!("{}{}", prefix, item.label), nav_area.width as usize - 4);
        nav_text.lines.push(Line::from(Span::styled(shown, style)));
    }
    if viewer.nav_items.is_empty() {
        nav_text.lines.push(Line::from(Span::styled("  (no nav)", Style::default().fg(Color::DarkGray))));
    }
    let nav_para = Paragraph::new(nav_text)
        .block(Block::default().title(" Nav ").borders(Borders::ALL).border_style(nav_border).style(Style::default().bg(Color::Rgb(0x1a, 0x1a, 0x22))))
        .wrap(Wrap { trim: false });
    f.render_widget(nav_para, nav_area);

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
    let mut md_text = wiki_render_markdown(&md);

    // Extract search text from active nav item for content highlighting
    let active_item = viewer.nav_items.get(sel);
    let search = match active_item.map(|it| &it.kind) {
        Some(crate::app_state::NavItemKind::Header) => {
            // Strip leading indent + # markers to get bare heading text
            active_item.map(|it| it.label.trim().trim_start_matches('#').trim().to_string()).unwrap_or_default()
        }
        Some(crate::app_state::NavItemKind::Link) => {
            // Strip "→ " prefix
            active_item.map(|it| it.label.trim_start_matches("→ ").trim().to_string()).unwrap_or_default()
        }
        _ => String::new(),
    };

    // Post-style: highlight matching text or the line near scroll start for the selected nav target
    let start = viewer.scroll;
    let max = (content_area.height as usize).saturating_sub(4);
    for (src_idx, line) in md_text.lines.iter_mut().enumerate().skip(start).take(max) {
        let is_active_region = if !search.is_empty() {
            line.spans.iter().any(|s| s.content.contains(&search))
        } else {
            // fallback: highlight around current scroll position
            src_idx == start || src_idx == start.saturating_add(1)
        };
        for span in &mut line.spans {
            let matches = !search.is_empty() && span.content.contains(&search);
            if matches || is_active_region {
                let mut st = span.style;
                st = st.fg(Color::Magenta).add_modifier(Modifier::BOLD | Modifier::UNDERLINED | Modifier::REVERSED);
                span.style = st;
            }
        }
    }

    for line in md_text.lines.into_iter().skip(start).take(max) {
        let tline = Line::from(
            line.spans.into_iter().map(|s| {
                Span::styled(truncate_str(&s.content, content_area.width as usize - 4), s.style)
            }).collect::<Vec<_>>()
        );
        content_text.lines.push(tline);
    }

    let content_para = Paragraph::new(content_text)
        .block(Block::default().title(" Wiki ").borders(Borders::ALL).border_style(content_border).style(Style::default().bg(Color::Rgb(0x1a, 0x1a, 0x22))))
        .wrap(Wrap { trim: false });
    f.render_widget(content_para, content_area);
}

pub fn draw_picker(f: &mut Frame, area: Rect, data: &PickerDrawData<'_>) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(28), Constraint::Percentage(28), Constraint::Percentage(44)])
        .split(area);

    draw_workspace_column(f, cols[0], data.workspaces, data.selected_workspace, data.focus == crate::app_state::PickerFocus::Workspaces);
    draw_sessions_column(f, cols[1], data.sessions, data.selected_session, data.focus == crate::app_state::PickerFocus::Sessions);
    draw_session_summary(f, cols[2], data.summary, data.summary_scroll, data.focus == crate::app_state::PickerFocus::Summary, data.wiki_links, data.active_link_idx, data.summary_action);

    // subtle hint line at bottom of area if space
    if area.height > 4 {
        let hint = " ←/→ focus Summary  w: toggle Wiki  Enter: full Wiki viewer or Launch  a/n/d  (right on Summary -> wiki view)";
        let hint_area = Rect { y: area.y + area.height - 1, height: 1, ..area };
        f.render_widget(
            Paragraph::new(Span::styled(hint, Style::default().fg(Color::DarkGray))),
            hint_area,
        );
    }
}

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
                Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD)
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
                .style(Style::default().bg(Color::Rgb(0x1a, 0x1a, 0x22))),
        )
        .wrap(Wrap { trim: false });
    f.render_widget(para, area);
}

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
                Style::default().fg(Color::Black).bg(Color::Rgb(0x80, 0xd0, 0xff)).add_modifier(Modifier::BOLD)
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
                .style(Style::default().bg(Color::Rgb(0x1a, 0x1a, 0x22))),
        )
        .wrap(Wrap { trim: false });
    f.render_widget(para, area);
}

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
    wiki_links: &[crate::app_state::WikiLink],
    active_link_idx: usize,
    summary_action: crate::app_state::SummaryAction,
) {
    // Reserve bottom 3 rows for buttons
    let button_height = 3u16;
    let content_height = area.height.saturating_sub(button_height);
    let content_area = Rect {
        x: area.x,
        y: area.y,
        width: area.width,
        height: content_height,
    };
    let buttons_area = Rect {
        x: area.x,
        y: area.y + content_height,
        width: area.width,
        height: button_height,
    };

    let is_wiki_mode = summary.trim_start().starts_with("--- ") || (focused && summary_action == crate::app_state::SummaryAction::ViewWiki);

    let mut text = Text::default();

    let start = scroll;
    let max_lines = (content_area.height as usize).saturating_sub(1);

    if is_wiki_mode {
        // Pure wiki markdown content (no session meta text)
        let md_text = wiki_render_markdown(summary);
        // Determine which link text is the active one (for highlight)
        let active_link_text = if active_link_idx < wiki_links.len() {
            wiki_links.get(active_link_idx).map(|l| l.text.as_str()).unwrap_or("")
        } else {
            "" // button is focused, no link highlight
        };

        for (idx, mut raw_line) in md_text.lines.into_iter().skip(start).take(max_lines).enumerate() {
            let _this_line = start + idx;

            // Highlight links by matching span content against known link texts
            for span in &mut raw_line.spans {
                for link in wiki_links.iter() {
                    if span.content.contains(&link.text) {
                        if link.text == active_link_text && !active_link_text.is_empty() {
                            span.style = Style::default()
                                .fg(Color::Magenta)
                                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED | Modifier::REVERSED);
                        } else {
                            span.style = Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::UNDERLINED);
                        }
                        break;
                    }
                }
            }

            // Make bold more visible (combine with color)
            for span in &mut raw_line.spans {
                if span.style.add_modifier.contains(Modifier::BOLD) {
                    if span.style.fg.is_none() || span.style.fg == Some(Color::Reset) {
                        span.style = span.style.fg(Color::LightCyan);
                    }
                }
            }

            text.lines.push(Line::from(
                raw_line.spans.into_iter()
                    .map(|s| Span::styled(s.content.to_string(), s.style))
                    .collect::<Vec<Span>>()
            ));
        }
    } else {
        // Session meta text (when Wiki button not active)
        text.lines.push(Line::from(Span::styled(
            "  Session Summary",
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        )));
        text.lines.push(Line::from(""));

        let all_lines: Vec<String> = if summary.is_empty() {
            vec!["  (no session)".to_string()]
        } else {
            summary.lines().map(|l| l.to_string()).collect()
        };
        let visible: Vec<_> = all_lines.iter().skip(start).take(max_lines).collect();

        let active_line = wiki_links.get(active_link_idx).map(|l| l.line).unwrap_or(usize::MAX);
        let wiki_section_start = all_lines.iter().position(|l| l.starts_with("--- ")).unwrap_or(0);

        for (vis_idx, line) in visible.iter().enumerate() {
            let global_line_in_all = start + vis_idx;
            let is_wiki_line = global_line_in_all >= wiki_section_start;
            let wiki_line_num = if is_wiki_line {
                global_line_in_all.saturating_sub(wiki_section_start + 1)
            } else {
                usize::MAX
            };

            let base_style = Style::default().fg(Color::Rgb(0xcc, 0xcc, 0xdd));

            let mut spans = vec![];
            let mut remaining = line.as_str();

            let line_links: Vec<_> = wiki_links.iter().filter(|l| l.line == wiki_line_num).collect();

            for link in &line_links {
                if let Some(text_pos) = remaining.find(&link.text) {
                    let mut link_start = text_pos;
                    while link_start > 0 && remaining.as_bytes().get(link_start - 1) != Some(&b'[') {
                        link_start -= 1;
                    }
                    if link_start > 0 && remaining.as_bytes().get(link_start - 1) == Some(&b'[') {
                        link_start -= 1;
                    }

                    let mut link_end = text_pos + link.text.len();
                    while link_end < remaining.len() && remaining.as_bytes().get(link_end) != Some(&b')') {
                        link_end += 1;
                    }
                    if link_end < remaining.len() && remaining.as_bytes().get(link_end) == Some(&b')') {
                        link_end += 1;
                    }

                    if link_start < text_pos + link.text.len() && link_end > link_start {
                        if link_start > 0 {
                            spans.push(Span::styled(remaining[..link_start].to_string(), base_style));
                        }
                        let full_link = &remaining[link_start..link_end];
                        let is_active = wiki_line_num == active_line;
                        let link_style = if is_active {
                            Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD | Modifier::UNDERLINED | Modifier::REVERSED)
                        } else {
                            Style::default().fg(Color::Cyan).add_modifier(Modifier::UNDERLINED)
                        };
                        spans.push(Span::styled(full_link.to_string(), link_style));
                        remaining = &remaining[link_end..];
                    }
                }
            }
            if !remaining.is_empty() {
                spans.push(Span::styled(remaining.to_string(), base_style));
            }

            if spans.is_empty() {
                let mut style = base_style;
                if is_wiki_line && wiki_line_num == active_line {
                    style = Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
                } else if is_wiki_line && !line_links.is_empty() {
                    style = Style::default().fg(Color::Cyan).add_modifier(Modifier::UNDERLINED);
                }
                text.lines.push(Line::from(Span::styled(
                    truncate_str(line, content_area.width as usize - 2),
                    style
                )));
            } else {
                text.lines.push(Line::from(spans));
            }
        }
    }

    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::Rgb(0x55, 0x55, 0x66))
    };
    let title = if is_wiki_mode { " Wiki " } else { " Summary " };
    let para = Paragraph::new(text)
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(border_style)
                .style(Style::default().bg(Color::Rgb(0x1a, 0x1a, 0x22))),
        )
        .wrap(Wrap { trim: false });
    f.render_widget(para, content_area);

    // Render buttons at the bottom
    let button_width = buttons_area.width / 2;
    let wiki_area = Rect {
        x: buttons_area.x,
        y: buttons_area.y,
        width: button_width,
        height: buttons_area.height,
    };
    let launch_area = Rect {
        x: buttons_area.x + button_width,
        y: buttons_area.y,
        width: buttons_area.width - button_width,
        height: buttons_area.height,
    };

    // Button focus is driven by active_link_idx:
    // idx == wiki_links.len()     => Wiki button focused
    // idx == wiki_links.len() + 1 => Launch button focused
    let n_links = wiki_links.len();
    let wiki_btn_focused = focused && active_link_idx == n_links;
    let launch_btn_focused = focused && active_link_idx == n_links + 1;

    draw_button(f, wiki_area, "Wiki", wiki_btn_focused);
    draw_button(f, launch_area, "Launch", launch_btn_focused);
}

pub fn draw_splash(f: &mut Frame, area: Rect, data: &SplashData<'_>) {
    let mut tmp = Buffer::empty(area);
    render_splash_to_buffer(&mut tmp, area, data);
    f.render_widget(
        BlitWidget {
            src: tmp,
            rel_x: 0,
            rel_y: 0,
        },
        area,
    );
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
        .border_style(Style::default().fg(Color::Rgb(0xc0, 0x80, 0xff)));

    let inner = block.inner(area);
    block.render(area, buf);

    if inner.width < 8 || inner.height < 6 {
        return;
    }

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(42), Constraint::Percentage(58)])
        .split(inner);

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
        let (prefix_style, body_style) = conversation_entry_styles(entry);
        let lines_iter: Vec<&str> = entry.lines().collect();
        for (li, line) in lines_iter.iter().enumerate() {
            let is_blank = line.trim().is_empty();
            if is_blank {
                consecutive_blanks += 1;
                if consecutive_blanks > 2 {
                    continue; // collapse excessive blank lines in display
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
    let line_count = text.lines.len() as u16;
    *last_line_count = line_count;
    *scroll = (*scroll).min(line_count.saturating_sub(1));

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
                .padding(ratatui::widgets::Padding::new(1, 1, 0, 0)),
        )
        .wrap(Wrap { trim: false })
        .scroll((*scroll, 0))
        .render(area, buf);
}
