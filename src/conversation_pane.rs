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
pub fn build_conversation_text(
    left_committed: &[String],
    current_response: &str,
    highlight_line: Option<usize>,
) -> Text<'static> {
    let mut left_text = Text::default();
    let mut line_idx = 0usize;
    let mut consecutive_blanks = 0usize;

    for (i, entry) in left_committed.iter().enumerate() {
        if is_plan_entry_confirm(entry) {
            push_attention_entry(
                &mut left_text,
                entry,
                highlight_line,
                &mut line_idx,
                PLAN_ORANGE,
            );
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
                        sp.style = highlight_style();
                    }
                }
                let is_blank = line.spans.is_empty()
                    || line.spans.iter().all(|s| s.content.trim().is_empty());
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
                highlight_style()
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