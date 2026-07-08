//! Shared styling and scroll math for selectable list panes (nav, picker tree).

use ratatui::style::{Color, Style};

use crate::palette::ColorDepth;

/// Shared background for selected list rows and markdown nav highlights (24-bit).
pub const LIST_SELECTION_BG: Color = Color::Rgb(0x2a, 0x2a, 0x34);

const LIST_SELECTION_FG: Color = Color::Rgb(0xde, 0xde, 0xe6);

/// xterm grayscale ramp indices for restricted terminals (see `palette::gray_rgb`).
const LIST_SELECTION_BG_256: u8 = 236; // rgb(48, 48, 48)
const LIST_SELECTION_FG_256: u8 = 254; // rgb(228, 228, 228)

/// Soft highlighted row / inline match (dark gray bg, light text).
pub fn list_selection_style() -> Style {
    match crate::palette::depth() {
        ColorDepth::True => Style::default().fg(LIST_SELECTION_FG).bg(LIST_SELECTION_BG),
        ColorDepth::Indexed256 => Style::default()
            .fg(Color::Indexed(LIST_SELECTION_FG_256))
            .bg(Color::Indexed(LIST_SELECTION_BG_256)),
        ColorDepth::Ansi16 | ColorDepth::None => Style::default()
            .fg(Color::White)
            .bg(Color::DarkGray),
    }
}

/// Scroll offset so `selected` stays near the vertical center of a fixed-height viewport.
pub fn list_visible_offset(selected: usize, visible: usize, total: usize) -> usize {
    if total <= visible || selected < visible / 2 {
        0
    } else if selected + visible / 2 >= total {
        total.saturating_sub(visible)
    } else {
        selected.saturating_sub(visible / 2)
    }
}

/// Cyan when focused, muted gray when not.
pub fn list_focus_border_style(focused: bool) -> Style {
    if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::Rgb(0x55, 0x55, 0x66))
    }
}

/// Default row style for a non-selected item in a focusable list.
pub fn list_unselected_style(focused: bool) -> Style {
    if focused {
        Style::default().fg(Color::White)
    } else {
        Style::default().fg(Color::Rgb(0xaa, 0xaa, 0xaa))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visible_offset_keeps_selection_centered() {
        assert_eq!(list_visible_offset(0, 5, 20), 0);
        assert_eq!(list_visible_offset(10, 5, 20), 8);
        assert_eq!(list_visible_offset(18, 5, 20), 15);
    }
}