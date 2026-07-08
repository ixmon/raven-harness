//! Purpose-built markdown → ratatui `Text` renderer for wiki and conversation panes.

use ratatui::{
    style::Style,
    text::{Line, Span, Text},
};

// ── Raven markdown renderer ──────────────────────────────────────────────────
// Replaces tui_markdown with a purpose-built renderer that directly produces
// ratatui types. Handles: headings, bold/italic/code, links, lists, code
// blocks, blockquotes, tables (box-drawn), and horizontal rules.

/// Styles for our dark TUI theme (#1a1a22 background).
pub mod md_style {
    use ratatui::style::{Color, Modifier, Style};
    pub fn h1() -> Style { Style::default().fg(Color::Rgb(0xff, 0xd0, 0x60)).add_modifier(Modifier::BOLD | Modifier::UNDERLINED) }
    pub fn h2() -> Style { Style::default().fg(Color::Rgb(0xe0, 0xb0, 0x50)).add_modifier(Modifier::BOLD) }
    pub fn h3() -> Style { Style::default().fg(Color::Rgb(0xc0, 0xa0, 0x60)).add_modifier(Modifier::ITALIC) }
    pub fn h_other() -> Style { Style::default().fg(Color::Rgb(0xa0, 0x90, 0x60)).add_modifier(Modifier::ITALIC) }
    pub fn bold() -> Style { Style::default().add_modifier(Modifier::BOLD) }
    pub fn italic() -> Style { Style::default().add_modifier(Modifier::ITALIC) }
    pub fn bold_italic() -> Style { Style::default().add_modifier(Modifier::BOLD | Modifier::ITALIC) }
    pub fn code() -> Style {
        Style::default()
            .fg(Color::Rgb(0xc0, 0xc0, 0xd0))
            .bg(Color::Rgb(0x1a, 0x1a, 0x24))
    }
    pub fn link() -> Style { Style::default().fg(Color::Cyan).add_modifier(Modifier::UNDERLINED) }
    pub fn blockquote() -> Style { Style::default().fg(Color::Rgb(0x80, 0xb0, 0x80)).add_modifier(Modifier::ITALIC) }
    pub fn table_border() -> Style { Style::default().fg(Color::DarkGray) }
    pub fn table_header() -> Style { Style::default().fg(Color::Rgb(0xff, 0xd0, 0x60)).add_modifier(Modifier::BOLD) }
    pub fn rule() -> Style { Style::default().fg(Color::DarkGray) }
    pub fn list_marker() -> Style { Style::default().fg(Color::DarkGray) }
}

pub fn render_markdown(md: &str) -> Text<'static> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let src_lines: Vec<&str> = md.lines().collect();
    let n = src_lines.len();
    let mut i = 0;
    let mut code_block: Option<(String, Vec<String>)> = None;

    while i < n {
        let raw = src_lines[i];

        // ── Code blocks ──
        if raw.trim_start().starts_with("```") {
            if let Some((lang, body)) = code_block.take() {
                let body_refs: Vec<&str> = body.iter().map(String::as_str).collect();
                lines.extend(crate::code_highlight::highlight_fenced_block(&lang, &body_refs));
                i += 1;
                continue;
            }
            let lang = raw.trim_start().trim_start_matches('`').trim().to_string();
            code_block = Some((lang, Vec::new()));
            i += 1;
            continue;
        }
        if let Some((_, ref mut body)) = code_block {
            body.push(raw.to_string());
            i += 1;
            continue;
        }

        let trimmed = raw.trim();

        // ── Empty line ──
        if trimmed.is_empty() {
            lines.push(Line::default());
            i += 1;
            continue;
        }

        // ── Horizontal rule ──
        if (trimmed.starts_with("---") || trimmed.starts_with("***") || trimmed.starts_with("___"))
            && trimmed.chars().all(|c| c == '-' || c == '*' || c == '_' || c == ' ')
            && trimmed.len() >= 3
        {
            lines.push(Line::from(Span::styled("────────────────────────────────", md_style::rule())));
            i += 1;
            continue;
        }

        // ── Tables ──
        if is_table_row(raw) && i + 1 < n && is_separator_row(src_lines[i + 1]) {
            let table_lines = render_table_lines(&src_lines, &mut i);
            lines.extend(table_lines);
            continue;
        }

        // ── Headings ──
        if trimmed.starts_with('#') {
            let level = trimmed.chars().take_while(|c| *c == '#').count();
            let text = trimmed[level..].trim();
            let style = match level {
                1 => md_style::h1(),
                2 => md_style::h2(),
                3 => md_style::h3(),
                _ => md_style::h_other(),
            };
            if !lines.is_empty() {
                lines.push(Line::default()); // spacing before heading
            }
            lines.push(Line::from(Span::styled(text.to_string(), style)));
            i += 1;
            continue;
        }

        // ── Blockquotes ──
        if let Some(stripped) = trimmed.strip_prefix('>') {
            let content = stripped.trim_start();
            let mut spans = vec![Span::styled("▎ ", md_style::blockquote())];
            spans.extend(parse_inline(content, md_style::blockquote()));
            lines.push(Line::from(spans));
            i += 1;
            continue;
        }

        // ── Unordered lists ──
        if trimmed.starts_with("- ") || trimmed.starts_with("* ") || trimmed.starts_with("+ ") {
            let indent = raw.len() - raw.trim_start().len();
            let content = &trimmed[2..];
            let prefix = " ".repeat(indent) + "• ";
            let mut spans = vec![Span::styled(prefix, md_style::list_marker())];
            spans.extend(parse_inline(content, Style::default()));
            lines.push(Line::from(spans));
            i += 1;
            continue;
        }

        // ── Ordered lists ──
        if let Some(rest) = try_ordered_list(trimmed) {
            let indent = raw.len() - raw.trim_start().len();
            // Find the number prefix
            let num_end = trimmed.find('.').unwrap_or(0);
            let num = &trimmed[..num_end + 1];
            let prefix = " ".repeat(indent) + num + " ";
            let mut spans = vec![Span::styled(prefix, md_style::list_marker())];
            spans.extend(parse_inline(rest, Style::default()));
            lines.push(Line::from(spans));
            i += 1;
            continue;
        }

        // ── Normal paragraph text ──
        let spans = parse_inline(trimmed, Style::default());
        lines.push(Line::from(spans));
        i += 1;
    }

    if let Some((lang, body)) = code_block.take() {
        let body_refs: Vec<&str> = body.iter().map(String::as_str).collect();
        lines.extend(crate::code_highlight::highlight_fenced_block(&lang, &body_refs));
    }

    Text::from(lines)
}

/// Parse inline markdown formatting: **bold**, *italic*, ***both***, `code`, [links](url)
fn parse_inline(text: &str, base_style: Style) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let mut pos = 0;
    let mut buf = String::new();

    while pos < len {
        // ── Inline code: `...` ──
        if chars[pos] == '`' {
            if !buf.is_empty() {
                spans.push(Span::styled(std::mem::take(&mut buf), base_style));
            }
            pos += 1;
            let mut code = String::new();
            while pos < len && chars[pos] != '`' {
                code.push(chars[pos]);
                pos += 1;
            }
            if pos < len { pos += 1; } // skip closing `
            spans.push(Span::styled(code, md_style::code()));
            continue;
        }

        // ── Links: [text](url) ──
        if chars[pos] == '[' {
            // Look ahead for ](url)
            if let Some((link_text, url, end_pos)) = try_parse_link(&chars, pos) {
                if !buf.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut buf), base_style));
                }
                spans.push(Span::styled(link_text, md_style::link()));
                pos = end_pos;
                // Store url in a dimmed span for reference
                let _ = url; // We display link text styled, url is implicit for TUI
                continue;
            }
        }

        // ── Bold+italic: ***...*** ──
        if pos + 2 < len && chars[pos] == '*' && chars[pos+1] == '*' && chars[pos+2] == '*' {
            if !buf.is_empty() {
                spans.push(Span::styled(std::mem::take(&mut buf), base_style));
            }
            pos += 3;
            let mut inner = String::new();
            while pos + 2 < len && !(chars[pos] == '*' && chars[pos+1] == '*' && chars[pos+2] == '*') {
                inner.push(chars[pos]);
                pos += 1;
            }
            if pos + 2 < len { pos += 3; }
            spans.push(Span::styled(inner, md_style::bold_italic()));
            continue;
        }

        // ── Bold: **...** ──
        if pos + 1 < len && chars[pos] == '*' && chars[pos+1] == '*' {
            if !buf.is_empty() {
                spans.push(Span::styled(std::mem::take(&mut buf), base_style));
            }
            pos += 2;
            let mut inner = String::new();
            while pos + 1 < len && !(chars[pos] == '*' && chars[pos+1] == '*') {
                inner.push(chars[pos]);
                pos += 1;
            }
            if pos + 1 < len { pos += 2; }
            spans.push(Span::styled(inner, md_style::bold()));
            continue;
        }

        // ── Italic: *...* (single) ──
        if chars[pos] == '*' && (pos + 1 < len && chars[pos+1] != '*') {
            if !buf.is_empty() {
                spans.push(Span::styled(std::mem::take(&mut buf), base_style));
            }
            pos += 1;
            let mut inner = String::new();
            while pos < len && chars[pos] != '*' {
                inner.push(chars[pos]);
                pos += 1;
            }
            if pos < len { pos += 1; }
            spans.push(Span::styled(inner, md_style::italic()));
            continue;
        }

        buf.push(chars[pos]);
        pos += 1;
    }

    if !buf.is_empty() {
        spans.push(Span::styled(buf, base_style));
    }
    spans
}

/// Try to parse a markdown link at position `start` (which should be '[').
/// Returns (link_text, url, end_position) if successful.
fn try_parse_link(chars: &[char], start: usize) -> Option<(String, String, usize)> {
    let len = chars.len();
    if start >= len || chars[start] != '[' { return None; }
    let mut pos = start + 1;
    let mut text = String::new();
    // Find closing ]
    while pos < len && chars[pos] != ']' {
        text.push(chars[pos]);
        pos += 1;
    }
    if pos >= len { return None; }
    pos += 1; // skip ]
    // Expect (
    if pos >= len || chars[pos] != '(' { return None; }
    pos += 1;
    let mut url = String::new();
    while pos < len && chars[pos] != ')' {
        url.push(chars[pos]);
        pos += 1;
    }
    if pos >= len { return None; }
    pos += 1; // skip )
    Some((text, url, pos))
}

/// Try to match an ordered list item: "1. text", "2. text", etc.
fn try_ordered_list(trimmed: &str) -> Option<&str> {
    let dot_pos = trimmed.find('.')?;
    let num_part = &trimmed[..dot_pos];
    if num_part.chars().all(|c| c.is_ascii_digit()) && !num_part.is_empty() {
        let rest = &trimmed[dot_pos + 1..];
        if rest.starts_with(' ') {
            return Some(rest.trim_start());
        }
    }
    None
}

// ── Table rendering ──────────────────────────────────────────────────────────

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

/// Render a markdown table starting at `src_lines[*i]` into styled Lines.
/// Advances `*i` past the table.
fn render_table_lines(src_lines: &[&str], i: &mut usize) -> Vec<Line<'static>> {
    let mut rows: Vec<Vec<String>> = vec![];
    rows.push(parse_table_row(src_lines[*i]));
    *i += 1; // skip header
    *i += 1; // skip separator
    while *i < src_lines.len() && is_table_row(src_lines[*i]) && !is_separator_row(src_lines[*i]) {
        rows.push(parse_table_row(src_lines[*i]));
        *i += 1;
    }

    if rows.is_empty() { return vec![]; }
    let ncols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    let mut widths = vec![0usize; ncols];
    for row in &rows {
        for (j, cell) in row.iter().enumerate() {
            if j < ncols { widths[j] = widths[j].max(cell.len()); }
        }
    }
    for w in &mut widths { *w = (*w).max(3); }

    let border = md_style::table_border();
    let header_style = md_style::table_header();
    let mut out: Vec<Line<'static>> = Vec::new();

    // Top border: ┌───┬───┐
    let mut top = String::from("┌");
    for (j, w) in widths.iter().enumerate() {
        top.push_str(&"─".repeat(*w + 2));
        if j + 1 < ncols { top.push('┬'); }
    }
    top.push('┐');
    out.push(Line::from(Span::styled(top, border)));

    for (ri, row) in rows.iter().enumerate() {
        let cell_style = if ri == 0 { header_style } else { Style::default() };
        let mut spans: Vec<Span<'static>> = vec![Span::styled("│", border)];
        for (j, w) in widths.iter().enumerate() {
            let cell = row.get(j).map(|s| s.as_str()).unwrap_or("");
            spans.push(Span::styled(format!(" {:<width$} ", cell, width = w), cell_style));
            spans.push(Span::styled("│", border));
        }
        out.push(Line::from(spans));

        // Header separator
        if ri == 0 && rows.len() > 1 {
            let mut sep = String::from("├");
            for (j, w) in widths.iter().enumerate() {
                sep.push_str(&"─".repeat(*w + 2));
                if j + 1 < ncols { sep.push('┼'); }
            }
            sep.push('┤');
            out.push(Line::from(Span::styled(sep, border)));
        }
    }

    // Bottom border: └───┴───┘
    let mut bot = String::from("└");
    for (j, w) in widths.iter().enumerate() {
        bot.push_str(&"─".repeat(*w + 2));
        if j + 1 < ncols { bot.push('┴'); }
    }
    bot.push('┘');
    out.push(Line::from(Span::styled(bot, border)));

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_markdown_bold_and_heading() {
        let text = render_markdown("## Goal\n\n**done**");
        assert!(text.lines.len() >= 2);
        let joined: String = text
            .lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect();
        assert!(joined.contains("Goal"));
        assert!(joined.contains("done"));
    }
}
