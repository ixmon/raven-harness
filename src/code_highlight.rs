//! Fenced code block syntax highlighting (syntect → ratatui spans).

use std::sync::OnceLock;

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, Style as SynStyle, ThemeSet};
use syntect::parsing::SyntaxSet;

use crate::palette;
use crate::md_render::md_style;

pub const CODE_INDENT: &str = "  ";
const THEME_NAME: &str = "base16-ocean.dark";

/// Resolved style for fenced code blocks (uniform bg degrades via `palette`).
pub fn code_block_style() -> Style {
    let base = md_style::code();
    Style {
        fg: base.fg.map(palette::resolve),
        bg: base.bg.map(palette::resolve),
        add_modifier: base.add_modifier,
        ..Default::default()
    }
}

/// Usable line width inside a bordered markdown pane.
pub fn markdown_content_width(area: Rect) -> usize {
    area.width.saturating_sub(2).max(1) as usize
}

fn line_char_width(line: &Line<'_>) -> usize {
    line.spans
        .iter()
        .map(|s| s.content.chars().count())
        .sum()
}

fn is_fenced_code_line(line: &Line<'_>) -> bool {
    let block_bg = code_block_style().bg;
    if !line.spans.iter().any(|s| s.style.bg == block_bg) {
        return false;
    }
    line.spans
        .first()
        .is_some_and(|s| s.content.as_ref().starts_with(CODE_INDENT))
}

/// Extend fenced code lines to `width` chars with a uniform block background.
pub fn pad_code_block_lines(text: &mut Text<'_>, width: usize) {
    if width == 0 {
        return;
    }
    let block = code_block_style();
    let Some(block_bg) = block.bg else {
        return;
    };
    for line in &mut text.lines {
        if !is_fenced_code_line(line) {
            continue;
        }
        for span in &mut line.spans {
            span.style = span.style.bg(block_bg);
        }
        let used = line_char_width(line);
        if used < width {
            line.spans
                .push(Span::styled(" ".repeat(width - used), block));
        }
    }
}

fn syntax_set() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn theme() -> &'static syntect::highlighting::Theme {
    static THEME: OnceLock<syntect::highlighting::Theme> = OnceLock::new();
    THEME.get_or_init(|| {
        let ts = ThemeSet::load_defaults();
        ts.themes
            .get(THEME_NAME)
            .or_else(|| ts.themes.values().next())
            .cloned()
            .expect("syntect default themes loaded")
    })
}

fn resolve_syntax<'a>(ps: &'a SyntaxSet, lang: &str) -> &'a syntect::parsing::SyntaxReference {
    let token = lang.trim();
    if !token.is_empty() {
        if let Some(syn) = ps.find_syntax_by_token(token) {
            return syn;
        }
        if let Some(syn) = ps.find_syntax_by_extension(token) {
            return syn;
        }
        // Common aliases in agent output.
        let alias = match token {
            "py" => "python",
            "js" => "javascript",
            "ts" => "typescript",
            "tsx" => "typescript",
            "jsx" => "javascript",
            "sh" | "shell" | "zsh" => "bash",
            "yml" => "yaml",
            "rs" => "rust",
            "cpp" | "cc" | "hpp" => "cpp",
            "cs" => "c#",
            other => other,
        };
        if alias != token {
            if let Some(syn) = ps.find_syntax_by_token(alias) {
                return syn;
            }
            if let Some(syn) = ps.find_syntax_by_extension(alias) {
                return syn;
            }
        }
    }
    ps.find_syntax_plain_text()
}

fn syntect_color_to_ratatui(c: syntect::highlighting::Color) -> Color {
    palette::resolve(Color::Rgb(c.r, c.g, c.b))
}

fn style_from_syntect(syn: SynStyle) -> Style {
    let block = code_block_style();
    let mut modifiers = Modifier::empty();
    if syn.font_style.contains(FontStyle::BOLD) {
        modifiers |= Modifier::BOLD;
    }
    if syn.font_style.contains(FontStyle::ITALIC) {
        modifiers |= Modifier::ITALIC;
    }
    if syn.font_style.contains(FontStyle::UNDERLINE) {
        modifiers |= Modifier::UNDERLINED;
    }
    Style {
        fg: Some(syntect_color_to_ratatui(syn.foreground)),
        bg: block.bg,
        add_modifier: modifiers,
        ..Default::default()
    }
}

fn flat_code_line(line: &str) -> Line<'static> {
    let block = code_block_style();
    Line::from(vec![
        Span::styled(CODE_INDENT, block),
        Span::styled(line.to_string(), block),
    ])
}

/// Highlight a fenced code block. `lang` is the info string after ``` (may be empty).
pub fn highlight_fenced_block(lang: &str, body: &[&str]) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let lang_label = lang.trim();
    let block = code_block_style();
    if !lang_label.is_empty() {
        out.push(Line::from(Span::styled(
            format!("{CODE_INDENT}{lang_label}"),
            block,
        )));
    }

    if body.is_empty() {
        return out;
    }

    let ps = syntax_set();
    let syntax = resolve_syntax(ps, lang_label);
    let plain = syntax.name == "Plain Text";

    if plain {
        for line in body {
            out.push(flat_code_line(line));
        }
        return out;
    }

    let mut highlighter = HighlightLines::new(syntax, theme());
    for line in body {
        let regions = match highlighter.highlight_line(line, ps) {
            Ok(r) => r,
            Err(_) => {
                out.push(flat_code_line(line));
                continue;
            }
        };
        let mut spans = vec![Span::styled(CODE_INDENT, block)];
        for (style, text) in regions {
            if text.is_empty() {
                continue;
            }
            spans.push(Span::styled(
                text.to_string(),
                style_from_syntect(style),
            ));
        }
        if spans.len() == 1 {
            out.push(flat_code_line(line));
        } else {
            out.push(Line::from(spans));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;

    #[test]
    fn rust_block_uses_multiple_foreground_colors() {
        let md = "```rust\nfn main() {\n    let x = 1;\n}\n```";
        let text = crate::md_render::render_markdown(md);
        let fgs: Vec<Option<Color>> = text
            .lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.style.fg))
            .collect();
        let distinct: std::collections::HashSet<_> = fgs.iter().copied().collect();
        assert!(
            distinct.len() > 1,
            "expected syntax-highlighted spans, got {:?}",
            distinct
        );
    }

    #[test]
    fn unknown_lang_falls_back_to_flat_code() {
        let lines = highlight_fenced_block("", &["hello();"]);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].spans.iter().any(|s| s.content.contains("hello")));
    }

    #[test]
    fn pad_extends_code_block_to_full_width() {
        let md = "```rust\nlet x = 1;\n```";
        let mut text = crate::md_render::render_markdown(md);
        pad_code_block_lines(&mut text, 40);
        let code_line = text
            .lines
            .iter()
            .find(|l| {
                is_fenced_code_line(l)
                    && l.spans.iter().any(|s| s.content.contains('x'))
            })
            .expect("code line");
        assert_eq!(line_char_width(code_line), 40);
        assert!(
            code_line
                .spans
                .last()
                .is_some_and(|s| s.content.chars().all(|c| c == ' '))
        );
    }

    #[test]
    fn pad_skips_non_code_paragraph_lines() {
        let mut text = crate::md_render::render_markdown("Hello\n\n```\ncode\n```");
        let before = line_char_width(&text.lines[0]);
        pad_code_block_lines(&mut text, 30);
        assert_eq!(line_char_width(&text.lines[0]), before);
    }
}