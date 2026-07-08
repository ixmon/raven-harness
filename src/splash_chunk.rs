//! Splash layout chunk: optional prose + Mermaid diagram → terminal text.

use ratatui::{
    style::{Color, Style},
    text::{Line, Span, Text},
};

const SPLASH_INTRO: &str = "\
This interface is a series of horizontal frames.
Use ↑↓ or j/k to select in a frame, ←→ or h/l to switch frames.";

/// Bundled default: intro blurb + fenced Mermaid diagram.
pub fn default_splash_chunk() -> String {
    format!(
        "{SPLASH_INTRO}\n\n```mermaid\n{}\n```",
        include_str!("../assets/splash_layout.mmd").trim()
    )
}

/// Extract Mermaid source from a chunk file (fenced block, or bare flowchart/graph).
pub fn extract_mermaid_source(chunk: &str) -> Option<&str> {
    if let Some(start) = chunk.find("```mermaid") {
        let rest = &chunk[start + "```mermaid".len()..];
        if let Some(end) = rest.find("```") {
            let inner = rest[..end].trim();
            if !inner.is_empty() {
                return Some(inner);
            }
        }
    }

    let trimmed = chunk.trim();
    if trimmed.starts_with("flowchart") || trimmed.starts_with("graph ") {
        return Some(trimmed);
    }

    for (i, line) in chunk.lines().enumerate() {
        let t = line.trim();
        if t.starts_with("flowchart") || t.starts_with("graph ") {
            let offset = chunk
                .lines()
                .take(i)
                .map(|l| l.len() + 1)
                .sum::<usize>();
            return Some(chunk[offset..].trim());
        }
    }

    None
}

pub fn splash_chunk_has_mermaid(chunk: &str) -> bool {
    extract_mermaid_source(chunk).is_some()
}

/// Prose lines outside the Mermaid block (empty lines preserved).
pub fn splash_chunk_prose_lines(chunk: &str) -> Vec<&str> {
    if chunk.find("```mermaid").is_some() {
        let before = chunk.split("```mermaid").next().unwrap_or("");
        return before.lines().collect();
    }

    if let Some(mermaid) = extract_mermaid_source(chunk) {
        if chunk.trim() == mermaid {
            return vec![];
        }
        if let Some(pos) = chunk.find(mermaid) {
            return chunk[..pos].lines().collect();
        }
    }

    if splash_chunk_has_mermaid(chunk) {
        vec![]
    } else {
        chunk.lines().collect()
    }
}

pub fn render_mermaid_diagram(src: &str, width: u16) -> Result<String, mermaid_text::Error> {
    let max_w = usize::from(width.max(20));
    mermaid_text::render_with_width(src, Some(max_w))
}

fn hint_style() -> Style {
    Style::default().fg(Color::DarkGray)
}

fn diagram_style() -> Style {
    Style::default().fg(Color::Rgb(0x88, 0x88, 0x99))
}

fn is_splash_diagram_line(line: &str) -> bool {
    if line.contains('[') {
        return true;
    }
    if line.trim().starts_with("Screen ") {
        return true;
    }
    if line.chars().filter(|c| *c == '_').count() >= 8 {
        return true;
    }
    line.chars().any(|c| {
        matches!(
            c,
            '┌' | '└' | '│' | '─' | '┐' | '┘' | '├' | '┤' | '┬' | '┴' | '◄' | '►' | '▶' | '▼' | '▸' | '◂'
        )
    })
}

fn plain_line_style(line: &str) -> Style {
    let section = Style::default()
        .fg(Color::Cyan)
        .add_modifier(ratatui::style::Modifier::BOLD);
    if line.trim().starts_with("Screen ") {
        section
    } else if is_splash_diagram_line(line) {
        diagram_style()
    } else {
        hint_style()
    }
}

fn line_is_blank(line: &Line<'_>) -> bool {
    line.spans
        .iter()
        .map(|s| s.content.as_ref())
        .all(|s| s.chars().all(char::is_whitespace))
}

fn trim_blank_edges(lines: &mut Vec<Line<'static>>) {
    while lines.first().is_some_and(line_is_blank) {
        lines.remove(0);
    }
    while lines.last().is_some_and(line_is_blank) {
        lines.pop();
    }
}

pub fn build_splash_chunk_display(chunk: &str, area_width: u16) -> Text<'static> {
    let mut out: Vec<Line<'static>> = Vec::new();

    if let Some(mermaid) = extract_mermaid_source(chunk) {
        for line in splash_chunk_prose_lines(chunk) {
            if line.is_empty() {
                out.push(Line::from(""));
            } else {
                out.push(Line::from(Span::styled(
                    line.to_string(),
                    plain_line_style(line),
                )));
            }
        }
        trim_blank_edges(&mut out);
        if let Ok(rendered) = render_mermaid_diagram(mermaid, area_width) {
            let mut diagram_lines: Vec<Line<'static>> = rendered
                .lines()
                .map(|line| Line::from(Span::styled(line.to_string(), diagram_style())))
                .collect();
            trim_blank_edges(&mut diagram_lines);
            if !out.is_empty() && !diagram_lines.is_empty() {
                out.push(Line::from(""));
            }
            out.extend(diagram_lines);
            return Text::from(out);
        }
    }

    for line in chunk.lines() {
        if line.is_empty() {
            out.push(Line::from(""));
        } else {
            out.push(Line::from(Span::styled(
                line.to_string(),
                plain_line_style(line),
            )));
        }
    }
    Text::from(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_fenced_mermaid() {
        let chunk = "intro\n\n```mermaid\nflowchart LR\n  A --> B\n```\n";
        assert_eq!(
            extract_mermaid_source(chunk),
            Some("flowchart LR\n  A --> B")
        );
    }

    #[test]
    fn prose_lines_exclude_fence() {
        let chunk = "line one\n\n```mermaid\nflowchart LR\n  A --> B\n```";
        assert_eq!(splash_chunk_prose_lines(chunk), vec!["line one", ""]);
    }

    #[test]
    fn mermaid_output_has_no_leading_blank_lines() {
        let chunk = default_splash_chunk();
        let text = build_splash_chunk_display(&chunk, 70);
        let first_diagram = text
            .lines
            .iter()
            .position(|l| {
                let s: String = l.spans.iter().map(|x| x.content.as_ref()).collect();
                s.contains('┌') || s.contains('|')
            })
            .expect("diagram lines");
        assert!(
            first_diagram <= 3,
            "diagram should start soon after prose, got line {first_diagram}"
        );
    }

    #[test]
    fn default_chunk_renders_main_workspaces_browser() {
        let chunk = default_splash_chunk();
        let text = build_splash_chunk_display(&chunk, 72);
        let flat: String = text
            .lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert!(flat.contains("horizontal frames"));
        assert!(flat.contains("Main"));
        assert!(flat.contains("Workspaces"));
        assert!(flat.contains("Browser"));
        assert!(flat.contains("Coding Harness") || flat.contains("Harness"));
        assert!(flat.contains("Wiki"));
        assert!(flat.contains('↑') && flat.contains('↓'));
        assert!(
            !flat.contains("\"Main\""),
            "box labels should render without quote characters"
        );
    }
}