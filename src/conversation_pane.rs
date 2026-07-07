//! Build ratatui `Text` for the left conversation pane (committed + streaming).

use crate::md_render::render_markdown;
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
};

const PLAN_ORANGE: Color = Color::Rgb(0xff, 0xc0, 0x40);

fn is_plan_entry_confirm(entry: &str) -> bool {
    entry.trim().starts_with("Enter plan mode?")
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
        (
            Style::default().fg(Color::DarkGray),
            Style::default().fg(Color::DarkGray),
        )
    } else if entry.contains("enter plan mode")
        || entry.contains("Do you want to enter plan mode")
        || entry.contains("Would you like to enter plan mode")
    {
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

fn highlight_style() -> Style {
    Style::default()
        .fg(Color::Black)
        .bg(Color::Magenta)
        .add_modifier(Modifier::BOLD)
}

/// Build styled conversation text from committed lines and an optional streaming tail.
///
/// Returns `(text, gutter_colors)` where `gutter_colors[i]` is the color for the
/// gutter bar on logical line `i`. The gutter is NOT embedded in the text — it is
/// painted as an overlay by the render function so that wrapped lines stay aligned.
pub fn build_conversation_text(
    left_committed: &[String],
    current_response: &str,
    highlight_line: Option<usize>,
) -> (Text<'static>, Vec<Color>) {
    let mut left_text = Text::default();
    let mut gutter_colors: Vec<Color> = Vec::new();
    let mut line_idx = 0usize;
    let mut consecutive_blanks = 0usize;

    for (i, entry) in left_committed.iter().enumerate() {
        let gutter_color = gutter_color_for(entry);
        if is_plan_entry_confirm(entry) {
            let style = if Some(line_idx) == highlight_line {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Magenta)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(PLAN_ORANGE).add_modifier(Modifier::BOLD)
            };
            for line in entry.lines() {
                left_text.lines.push(Line::from(Span::styled(line.to_string(), style)));
                gutter_colors.push(gutter_color);
                line_idx += 1;
            }
        } else if entry.starts_with("You: ")
            || entry.starts_with("> ")
            || entry.starts_with("You (interject")
        {
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
                    highlight_style()
                } else if li == 0 {
                    prefix_style
                } else {
                    body_style
                };
                left_text.lines.push(Line::from(Span::styled(line.to_string(), style)));
                gutter_colors.push(gutter_color);
                line_idx += 1;
            }
        } else {
            let md = render_markdown(entry);
            for mut line in md.lines {
                if Some(line_idx) == highlight_line {
                    for sp in &mut line.spans {
                        sp.style = highlight_style();
                    }
                }
                let is_blank = line.spans.is_empty()
                    || line.spans.iter().all(|s| s.content.trim().is_empty());
                if is_blank {
                    consecutive_blanks += 1;
                    if consecutive_blanks <= 2 {
                        left_text.lines.push(line);
                        gutter_colors.push(gutter_color);
                    }
                } else {
                    consecutive_blanks = 0;
                    left_text.lines.push(line);
                    gutter_colors.push(gutter_color);
                }
                line_idx += 1;
            }
        }
        if i < left_committed.len() - 1 && consecutive_blanks < 2 {
            left_text.lines.push(Line::from(""));
            gutter_colors.push(Color::Reset); // no gutter on separator lines
            line_idx += 1;
            consecutive_blanks += 1;
        }
    }

    if !current_response.is_empty() {
        let stream_gutter = Color::Rgb(0x40, 0xb0, 0x40);
        left_text.lines.push(Line::from(Span::styled(
            "Agent (streaming):",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD | Modifier::ITALIC),
        )));
        gutter_colors.push(stream_gutter);
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
                highlight_style()
            } else {
                Style::default().fg(Color::Rgb(0xd0, 0xf0, 0xd0))
            };
            left_text.lines.push(Line::from(Span::styled(line.to_string(), style)));
            gutter_colors.push(stream_gutter);
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
                gutter_colors.push(stream_gutter);
            }
        }
    }

    (left_text, gutter_colors)
}

/// Determine gutter bar color from the entry content.
pub fn gutter_color_for(entry: &str) -> Color {
    if entry.starts_with("You: ") || entry.starts_with("> ") || entry.starts_with("You (interject") {
        Color::Cyan
    } else if entry.starts_with("Agent: ") || entry.starts_with("Agent (partial): ") {
        Color::Rgb(0x40, 0xb0, 0x40)
    } else if entry.contains("ERROR") || entry.starts_with("⚠") {
        Color::Rgb(0xff, 0x60, 0x60)
    } else if entry.starts_with("✅")
        || entry.starts_with("⛔")
        || entry.starts_with("⏹")
        || entry.starts_with("🔒")
    {
        Color::Rgb(0xc0, 0xa0, 0x30)
    } else if entry.starts_with("Raven Hotel - Loaded session")
        || entry.starts_with("Use ↑/↓")
    {
        Color::Rgb(0x33, 0x33, 0x40)
    } else if entry.contains("enter plan mode")
        || entry.contains("Do you want to enter plan mode")
        || entry.contains("Would you like to enter plan mode")
        || is_plan_entry_confirm(entry)
    {
        PLAN_ORANGE
    } else {
        Color::Rgb(0x44, 0x44, 0x55)
    }
}