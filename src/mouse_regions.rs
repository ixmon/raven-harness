//! Clickable pane regions updated each frame during render.

use crate::desktop::ActiveDesktop;
use ratatui::layout::{Constraint, Direction, Layout, Rect};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MouseRegions {
    pub breadcrumb_bar: Rect,
    pub splash_magenta: Rect,
    pub splash_picker: Rect,
    pub picker_tree: Rect,
    pub picker_summary: Rect,
    pub overview_picker: Rect,
    pub overview_nav: Rect,
    pub overview_content: Rect,
    pub wiki_nav: Rect,
    pub wiki_content: Rect,
    pub input: Rect,
}

pub fn point_in(r: Rect, col: u16, row: u16) -> bool {
    r.width > 0
        && r.height > 0
        && col >= r.x
        && col < r.x + r.width
        && row >= r.y
        && row < r.y + r.height
}

/// Block inner rect (1-cell border on all sides).
pub fn block_inner(area: Rect) -> Rect {
    if area.width < 2 || area.height < 2 {
        return Rect::default();
    }
    Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width - 2,
        height: area.height - 2,
    }
}

/// Map a screen row inside a list pane to an item index.
pub fn list_item_at_row(
    pane: Rect,
    row: u16,
    top_gutter_lines: usize,
    scroll_offset: usize,
    item_count: usize,
) -> Option<usize> {
    if item_count == 0 {
        return None;
    }
    let inner = block_inner(pane);
    if row < inner.y || row >= inner.y + inner.height {
        return None;
    }
    let rel = row.saturating_sub(inner.y) as usize;
    if rel < top_gutter_lines {
        return None;
    }
    let idx = scroll_offset + rel.saturating_sub(top_gutter_lines);
    if idx < item_count {
        Some(idx)
    } else {
        None
    }
}

pub fn compute_mouse_regions(
    active: ActiveDesktop,
    content_area: Rect,
    showing_wiki_viewer: bool,
) -> MouseRegions {
    let mut regions = MouseRegions::default();
    if showing_wiki_viewer {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
            .split(content_area);
        regions.wiki_nav = cols[0];
        regions.wiki_content = cols[1];
        return regions;
    }
    match active {
        ActiveDesktop::Splash => {
            let cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
                .split(content_area);
            regions.splash_magenta = cols[0];
            regions.splash_picker = cols[1];
        }
        ActiveDesktop::Picker => {
            let cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
                .split(content_area);
            regions.picker_tree = cols[0];
            regions.picker_summary = cols[1];
        }
        ActiveDesktop::Overview => {
            let cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Percentage(30),
                    Constraint::Percentage(30),
                    Constraint::Percentage(40),
                ])
                .split(content_area);
            regions.overview_picker = cols[0];
            regions.overview_nav = cols[1];
            regions.overview_content = cols[2];
        }
        ActiveDesktop::Workspace | ActiveDesktop::WikiViewer => {}
    }
    regions
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_item_maps_row_with_gutter_and_scroll() {
        let pane = Rect::new(0, 0, 20, 10);
        // inner starts y=1; gutter=1 → first item at row 2 → index 0
        assert_eq!(list_item_at_row(pane, 2, 1, 0, 5), Some(0));
        assert_eq!(list_item_at_row(pane, 3, 1, 2, 5), Some(3));
        assert_eq!(list_item_at_row(pane, 1, 1, 0, 5), None);
    }

    #[test]
    fn point_in_respects_bounds() {
        let r = Rect::new(5, 5, 10, 4);
        assert!(point_in(r, 5, 5));
        assert!(!point_in(r, 15, 5));
    }
}