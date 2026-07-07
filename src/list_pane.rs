//! Shared styling and scroll math for selectable list panes (nav, picker tree).

use ratatui::style::{Color, Modifier, Style};

pub const LIST_SELECTION_BG: Color = Color::Rgb(0x20, 0x50, 0x80);

/// Bold white-on-blue style for the currently selected list row.
pub fn list_selection_style() -> Style {
    Style::default()
        .fg(Color::White)
        .bg(LIST_SELECTION_BG)
        .add_modifier(Modifier::BOLD)
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