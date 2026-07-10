//! Transient top-right notification toasts.
//!
//! Status messages that used to clutter the conversation or trace pane can
//! surface here briefly, then auto-dismiss.

use std::time::{Duration, Instant};

use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

/// Visual severity for border / accent color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Success,
    Warning,
    Error,
}

impl ToastKind {
    pub fn border_color(self) -> Color {
        match self {
            ToastKind::Info => Color::Cyan,
            ToastKind::Success => Color::Rgb(0x60, 0xd0, 0x80),
            ToastKind::Warning => Color::Rgb(0xff, 0xc0, 0x40),
            ToastKind::Error => Color::Rgb(0xff, 0x60, 0x60),
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            ToastKind::Info => " info ",
            ToastKind::Success => " ok ",
            ToastKind::Warning => " note ",
            ToastKind::Error => " error ",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Toast {
    pub message: String,
    pub kind: ToastKind,
    pub created_at: Instant,
    pub ttl: Duration,
}

impl Toast {
    pub fn new(message: impl Into<String>, kind: ToastKind) -> Self {
        Self {
            message: message.into(),
            kind,
            created_at: Instant::now(),
            ttl: Duration::from_secs(3),
        }
    }

    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    pub fn info(message: impl Into<String>) -> Self {
        Self::new(message, ToastKind::Info)
    }

    pub fn success(message: impl Into<String>) -> Self {
        Self::new(message, ToastKind::Success)
    }

    pub fn warning(message: impl Into<String>) -> Self {
        Self::new(message, ToastKind::Warning)
    }

    pub fn is_expired(&self) -> bool {
        self.created_at.elapsed() >= self.ttl
    }

    /// 0.0 (just shown) … 1.0 (about to expire). Used for subtle fade of the track.
    pub fn age_ratio(&self) -> f64 {
        let elapsed = self.created_at.elapsed().as_secs_f64();
        let ttl = self.ttl.as_secs_f64().max(0.001);
        (elapsed / ttl).clamp(0.0, 1.0)
    }
}

/// FIFO toast queue; only the newest toast is drawn (keeps chrome quiet).
#[derive(Debug, Default)]
pub struct ToastState {
    queue: Vec<Toast>,
}

impl ToastState {
    pub fn push(&mut self, toast: Toast) {
        self.queue.push(toast);
        // Cap backlog so a flood of status events can't queue forever.
        const MAX: usize = 6;
        if self.queue.len() > MAX {
            let drop_n = self.queue.len() - MAX;
            self.queue.drain(0..drop_n);
        }
    }

    /// Drop expired toasts. Returns true if the active toast changed.
    pub fn tick(&mut self) -> bool {
        let before = self.queue.len();
        self.queue.retain(|t| !t.is_expired());
        before != self.queue.len()
    }

    pub fn active(&self) -> Option<&Toast> {
        self.queue.first()
    }

    /// True while any toast is visible — event loop should keep redrawing.
    pub fn needs_redraw(&self) -> bool {
        !self.queue.is_empty()
    }
}

/// Infer kind from common settings / status phrasing.
pub fn infer_kind(message: &str) -> ToastKind {
    let lower = message.to_ascii_lowercase();
    if lower.contains("error")
        || lower.contains("failed")
        || lower.contains("could not")
        || lower.contains("unable")
    {
        ToastKind::Error
    } else if lower.contains("removed")
        || lower.contains("deleted")
        || lower.contains("not configured")
        || lower.contains("did not decrypt")
        || lower.contains("falling back")
        || lower.contains("warning")
    {
        ToastKind::Warning
    } else if lower.contains("saved")
        || lower.contains("loaded")
        || lower.contains("updated")
        || lower.contains("added")
        || lower.contains("ok")
        || lower.contains("success")
    {
        ToastKind::Success
    } else {
        ToastKind::Info
    }
}

/// Top-right toast overlay. Safe to call every frame; no-op when empty.
pub fn draw_toasts(f: &mut Frame, screen: Rect, state: &ToastState) {
    let Some(toast) = state.active() else {
        return;
    };

    let msg = toast.message.trim();
    if msg.is_empty() {
        return;
    }

    let max_w = screen.width.saturating_sub(4).clamp(20, 56);
    // Rough wrap estimate for height.
    let chars = msg.chars().count() as u16;
    let content_cols = max_w.saturating_sub(4).max(8);
    let lines = (chars / content_cols).saturating_add(1).clamp(1, 4);
    let height = lines + 2; // borders

    let area = toast_area(screen, max_w, height);
    if area.width < 8 || area.height < 2 {
        return;
    }

    f.render_widget(Clear, area);

    let age = toast.age_ratio();
    let border = toast.kind.border_color();
    // Dim slightly as the toast ages (simple fade cue).
    let fg = if age > 0.75 {
        Color::DarkGray
    } else {
        Color::Rgb(0xdd, 0xdd, 0xee)
    };

    let block = Block::default()
        .title(Span::styled(
            toast.kind.title(),
            Style::default()
                .fg(border)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border))
        .style(Style::default().bg(Color::Rgb(0x12, 0x12, 0x1a)));

    let para = Paragraph::new(Line::from(Span::styled(
        msg.to_string(),
        Style::default().fg(fg),
    )))
    .block(block)
    .wrap(Wrap { trim: true })
    .alignment(Alignment::Left);

    f.render_widget(para, area);
}

fn toast_area(screen: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(screen.width.saturating_sub(2));
    let h = height.min(screen.height.saturating_sub(2));
    // Sit just under the breadcrumb row when present (row 1–2).
    let y = screen.y.saturating_add(1).min(screen.y + screen.height.saturating_sub(h));
    let x = screen
        .x
        .saturating_add(screen.width.saturating_sub(w.saturating_add(1)));
    Rect {
        x,
        y,
        width: w,
        height: h,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expires_after_ttl() {
        let mut t = Toast::info("hi").with_ttl(Duration::from_millis(1));
        t.created_at = Instant::now() - Duration::from_millis(5);
        assert!(t.is_expired());
    }

    #[test]
    fn queue_drops_expired_and_caps() {
        let mut s = ToastState::default();
        for i in 0..10 {
            s.push(Toast::info(format!("m{i}")));
        }
        assert!(s.queue.len() <= 6);
        assert!(s.active().is_some());
    }

    #[test]
    fn infer_kind_samples() {
        assert_eq!(infer_kind("Endpoint deleted."), ToastKind::Warning);
        assert_eq!(
            infer_kind("Brave Search: key loaded from vault/env"),
            ToastKind::Success
        );
        assert_eq!(
            infer_kind("Brave Search: not configured (using DuckDuckGo)"),
            ToastKind::Warning
        );
        assert_eq!(infer_kind("Failed to save key"), ToastKind::Error);
    }
}
