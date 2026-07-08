//! Cycling splash tips (upper-right of magenta pane on the first screen).

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
};

/// Full tip cycle length (~3.6s at a 30ms event-loop poll).
pub const TIP_CYCLE_TICKS: u32 = 120;

/// Fade steps (0 = fully visible, `TIP_FADE_STEPS` = invisible on black).
pub const TIP_FADE_STEPS: u8 = 10;

/// Ticks per fade step (~90ms).
pub const TIP_FADE_TICKS: u32 = 3;

/// Visible hold before fade-out begins.
pub const TIP_HOLD_TICKS: u32 =
    TIP_CYCLE_TICKS - 2 * (TIP_FADE_STEPS as u32 * TIP_FADE_TICKS);

/// Soft-wrap long lines at this width; author's `\n` breaks are preserved.
pub const TIP_WRAP_MAX_CHARS: usize = 90;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TipPhase {
    Hold,
    FadeOut,
    FadeIn,
}

#[derive(Debug, Clone)]
pub struct SplashTipsState {
    pub tips: Vec<String>,
    pub index: usize,
    phase: TipPhase,
    /// 0 = full brightness, `TIP_FADE_STEPS` = fully faded.
    fade_level: u8,
    tick: u32,
}

impl SplashTipsState {
    pub fn new(tips: Vec<String>) -> Self {
        Self {
            tips,
            index: 0,
            phase: TipPhase::Hold,
            fade_level: 0,
            tick: 0,
        }
    }

    pub fn current(&self) -> &str {
        self.tips.get(self.index).map(String::as_str).unwrap_or("")
    }

    pub fn fade_level(&self) -> u8 {
        self.fade_level
    }

    /// Advance the cycle timer. Returns `true` when a redraw is needed.
    pub fn tick(&mut self) -> bool {
        if self.tips.len() <= 1 {
            return false;
        }

        self.tick = self.tick.saturating_add(1);
        match self.phase {
            TipPhase::Hold => {
                if self.tick >= TIP_HOLD_TICKS {
                    self.tick = 0;
                    self.phase = TipPhase::FadeOut;
                }
                false
            }
            TipPhase::FadeOut => {
                if self.tick < TIP_FADE_TICKS {
                    return true;
                }
                self.tick = 0;
                if self.fade_level < TIP_FADE_STEPS {
                    self.fade_level += 1;
                }
                if self.fade_level >= TIP_FADE_STEPS {
                    self.index = (self.index + 1) % self.tips.len();
                    self.phase = TipPhase::FadeIn;
                }
                true
            }
            TipPhase::FadeIn => {
                if self.tick < TIP_FADE_TICKS {
                    return true;
                }
                self.tick = 0;
                if self.fade_level > 0 {
                    self.fade_level -= 1;
                }
                if self.fade_level == 0 {
                    self.phase = TipPhase::Hold;
                }
                true
            }
        }
    }

    pub fn should_animate(&self) -> bool {
        self.tips.len() > 1
    }
}

/// Load tips from `/tmp/tips`, else bundled `assets/splash_tips.txt`.
pub fn load_splash_tips() -> Vec<String> {
    if let Ok(s) = std::fs::read_to_string("/tmp/tips") {
        let parsed = parse_tips(&s);
        if !parsed.is_empty() {
            return parsed;
        }
    }
    parse_tips(include_str!("../assets/splash_tips.txt"))
}

/// Tips are separated by two or more blank lines in the source file.
pub fn parse_tips(content: &str) -> Vec<String> {
    let mut tips = Vec::new();
    let mut current = String::new();
    let mut blank_run = 0usize;

    for line in content.lines() {
        if line.trim().is_empty() {
            blank_run += 1;
            if blank_run >= 2 && !current.trim().is_empty() {
                tips.push(current.trim().to_string());
                current.clear();
            } else if blank_run == 1 && !current.is_empty() {
                current.push('\n');
            }
        } else {
            blank_run = 0;
            if !current.is_empty() && !current.ends_with('\n') {
                current.push('\n');
            }
            current.push_str(line);
        }
    }
    if !current.trim().is_empty() {
        tips.push(current.trim().to_string());
    }
    tips
}

fn tip_wrap_width(column_width: usize) -> usize {
    column_width.clamp(12, TIP_WRAP_MAX_CHARS)
}

fn wrap_tip_line(s: &str, width: usize) -> Vec<String> {
    let width = width.max(12);
    let words: Vec<&str> = s.split_whitespace().collect();
    if words.is_empty() {
        return vec![String::new()];
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in words {
        let extra = if current.is_empty() { 0 } else { 1 };
        if current.chars().count() + extra + word.chars().count() > width {
            if !current.is_empty() {
                lines.push(current);
                current = String::new();
            }
            if word.chars().count() > width {
                let mut rest = word;
                while !rest.is_empty() {
                    let end = rest
                        .char_indices()
                        .take(width)
                        .last()
                        .map(|(i, c)| i + c.len_utf8())
                        .unwrap_or(rest.len());
                    lines.push(rest[..end].to_string());
                    rest = &rest[end..];
                }
            } else {
                current = word.to_string();
            }
        } else if current.is_empty() {
            current = word.to_string();
        } else {
            current.push(' ');
            current.push_str(word);
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

fn color_to_rgb(c: Color) -> (u8, u8, u8) {
    match c {
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Black => (0, 0, 0),
        Color::White => (255, 255, 255),
        Color::DarkGray => (128, 128, 128),
        Color::Green => (0, 200, 0),
        Color::Cyan => (0, 200, 200),
        _ => (180, 180, 180),
    }
}

fn fade_color(c: Color, level: u8) -> Color {
    if level == 0 {
        return c;
    }
    let (r, g, b) = color_to_rgb(c);
    let t = f32::from(level) / f32::from(TIP_FADE_STEPS);
    let scale = 1.0 - t;
    Color::Rgb(
        (f32::from(r) * scale).round() as u8,
        (f32::from(g) * scale).round() as u8,
        (f32::from(b) * scale).round() as u8,
    )
}

fn fade_style(style: Style, level: u8) -> Style {
    if level == 0 {
        return style;
    }
    let mut out = style;
    if let Some(fg) = style.fg {
        out.fg = Some(fade_color(fg, level));
    }
    out
}

fn tip_url_style(level: u8) -> Style {
    fade_style(
        Style::default().fg(Color::Rgb(0xa0, 0xd0, 0xff)),
        level,
    )
}

fn tip_dollar_style(level: u8) -> Style {
    fade_style(
        Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD),
        level,
    )
}

fn consider_highlight(
    best: &mut Option<(usize, usize, Style)>,
    start: usize,
    len: usize,
    style: Style,
) {
    if len == 0 {
        return;
    }
    let dominates = match *best {
        None => true,
        Some((best_start, _, _)) => start < best_start,
    };
    if dominates {
        *best = Some((start, len, style));
    }
}

fn next_highlight(rest: &str, fade_level: u8) -> Option<(usize, usize, Style)> {
    let mut best: Option<(usize, usize, Style)> = None;

    for prefix in ["https://", "http://"] {
        if let Some(start) = rest.find(prefix) {
            let tail = &rest[start..];
            let url_len = tail
                .char_indices()
                .find(|(_, c)| c.is_whitespace())
                .map(|(i, _)| i)
                .unwrap_or(tail.len());
            consider_highlight(&mut best, start, url_len, tip_url_style(fade_level));
        }
    }

    if let Some(start) = rest.find("~/.raven-hotel/") {
        consider_highlight(
            &mut best,
            start,
            "~/.raven-hotel/".len(),
            fade_style(
                Style::default().fg(Color::Rgb(0xc0, 0x80, 0xff)),
                fade_level,
            ),
        );
    }

    if let Some(start) = rest.find("OpenAI") {
        consider_highlight(
            &mut best,
            start,
            "OpenAI".len(),
            fade_style(Style::default().fg(Color::Cyan), fade_level),
        );
    }

    if let Some(start) = rest.find('$') {
        consider_highlight(&mut best, start, 1, tip_dollar_style(fade_level));
    }

    best
}

fn styled_tip_line(line: &str, base: Style, fade_level: u8) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut rest = line;

    let base = fade_style(base, fade_level);

    while !rest.is_empty() {
        let Some((pos, hi_len, hi_style)) = next_highlight(rest, fade_level) else {
            spans.push(Span::styled(rest.to_string(), base));
            break;
        };

        if pos > 0 {
            spans.push(Span::styled(rest[..pos].to_string(), base));
        }
        spans.push(Span::styled(
            rest[pos..pos + hi_len].to_string(),
            hi_style,
        ));
        rest = &rest[pos + hi_len..];
    }

    if spans.is_empty() {
        spans.push(Span::styled(String::new(), base));
    }
    Line::from(spans)
}

fn line_is_blank(line: &Line<'_>) -> bool {
    line.spans
        .iter()
        .all(|s| s.content.chars().all(char::is_whitespace))
}

fn push_blank_line(out: &mut Vec<Line<'static>>) {
    if !out.last().is_some_and(line_is_blank) {
        out.push(Line::from(""));
    }
}

pub fn build_splash_tip_text(tip: &str, column_width: usize, fade_level: u8) -> Text<'static> {
    let hint = Style::default().fg(Color::DarkGray);
    let headline = Style::default()
        .fg(Color::White)
        .add_modifier(Modifier::BOLD);
    let fade_level = fade_level.min(TIP_FADE_STEPS);
    let wrap_width = tip_wrap_width(column_width);

    let mut out: Vec<Line<'static>> = Vec::new();
    let mut saw_content = false;
    let mut headline_used = false;

    for raw_line in tip.lines() {
        if raw_line.trim().is_empty() {
            if saw_content {
                push_blank_line(&mut out);
            }
            continue;
        }
        saw_content = true;
        let is_headline = !headline_used;
        if is_headline {
            headline_used = true;
        }
        let style = if is_headline { headline } else { hint };

        for wrapped in wrap_tip_line(raw_line.trim(), wrap_width) {
            out.push(styled_tip_line(&wrapped, style, fade_level));
        }
        if is_headline {
            push_blank_line(&mut out);
        }
    }

    if out.is_empty() {
        out.push(Line::from(Span::styled(
            "Raven Hotel — agent harness",
            headline,
        )));
    }
    Text::from(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_three_tips_from_blank_line_separators() {
        let tips = parse_tips(include_str!("../assets/splash_tips.txt"));
        assert_eq!(tips.len(), 3);
        assert!(tips[0].contains("Open source"));
        assert!(tips[0].contains("OpenAI"));
        assert!(tips[1].contains("Local means private"));
        assert!(tips[1].contains("~/.raven-hotel"));
        assert!(tips[1].contains("encrypted"));
        assert!(tips[2].contains("raven-harness"));
    }

    #[test]
    fn cycles_through_tips_with_fade() {
        let tips = vec!["a".into(), "b".into(), "c".into()];
        let mut state = SplashTipsState::new(tips);
        assert_eq!(state.current(), "a");
        assert_eq!(state.fade_level(), 0);

        for _ in 0..TIP_HOLD_TICKS {
            assert!(!state.tick());
        }
        assert_eq!(state.current(), "a");

        let fade_out_ticks = TIP_FADE_STEPS as u32 * TIP_FADE_TICKS;
        for _ in 0..fade_out_ticks {
            state.tick();
        }
        assert_eq!(state.fade_level(), TIP_FADE_STEPS);
        assert_eq!(state.current(), "b");

        let fade_in_ticks = TIP_FADE_STEPS as u32 * TIP_FADE_TICKS;
        for _ in 0..fade_in_ticks {
            state.tick();
        }
        assert_eq!(state.fade_level(), 0);
        assert_eq!(state.current(), "b");
    }

    #[test]
    fn blank_line_after_headline() {
        let tip = "Tip title\nBody starts here.";
        let text = build_splash_tip_text(tip, 80, 0);
        assert_eq!(text.lines[0].spans[0].content.as_ref(), "Tip title");
        assert!(line_is_blank(&text.lines[1]));
        assert!(text.lines[2].spans[0].content.contains("Body"));
    }

    #[test]
    fn preserves_author_line_breaks() {
        let tip = "Headline\n\nSentence one.\nSentence two.";
        let text = build_splash_tip_text(tip, 80, 0);
        assert!(text.lines.len() >= 4);
    }

    #[test]
    fn highlights_balance_dollar_green() {
        let line = styled_tip_line("Balance ($) here", Style::default(), 0);
        let dollar = line
            .spans
            .iter()
            .find(|s| s.content.contains('$'))
            .expect("dollar span");
        assert_eq!(dollar.style.fg, Some(Color::Green));
    }

    #[test]
    fn highlights_entire_url_blue() {
        let line = styled_tip_line(
            "see https://github.com/ixmon/raven-harness thanks",
            Style::default(),
            0,
        );
        let url: String = line
            .spans
            .iter()
            .filter(|s| s.style.fg == Some(Color::Rgb(0xa0, 0xd0, 0xff)))
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(url, "https://github.com/ixmon/raven-harness");
    }

    #[test]
    fn wrap_width_caps_at_ninety() {
        assert_eq!(tip_wrap_width(120), 90);
        assert_eq!(tip_wrap_width(40), 40);
    }

    #[test]
    fn fade_level_dims_headline() {
        let bright = build_splash_tip_text("Bright title\nBody.", 80, 0);
        let dim = build_splash_tip_text("Bright title\nBody.", 80, TIP_FADE_STEPS);
        let bright_fg = bright.lines[0].spans[0].style.fg;
        let dim_fg = dim.lines[0].spans[0].style.fg;
        assert_ne!(bright_fg, dim_fg);
        assert_eq!(dim_fg, Some(Color::Rgb(0, 0, 0)));
    }
}