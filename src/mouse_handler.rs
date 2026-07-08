//! Mouse click and wheel handling for pane focus, list selection, and scrolling.

use crate::app_state::{
    App, PickerFocus, Pane, SplashFocus, SummaryAction, ViewFocus,
};
use crate::desktop::ActiveDesktop;
use crate::list_pane::list_visible_offset;
use crate::mouse_regions::{block_inner, compute_mouse_regions, list_item_at_row, point_in, MouseRegions};
use crate::wiki_browser::WikiFocus;
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use raven_tui::agent::Agent;
use std::sync::Arc;
use tokio::sync::Mutex as TokioMutex;

pub fn update_mouse_regions(
    app: &mut App,
    content_area: ratatui::layout::Rect,
    input_area: ratatui::layout::Rect,
    breadcrumb_area: ratatui::layout::Rect,
    breadcrumb_data: &crate::tui_render::BreadcrumbData,
) {
    app.mouse_regions = compute_mouse_regions(
        app.desktop.active,
        content_area,
        app.desktop.showing_wiki_viewer(),
    );
    app.mouse_regions.input = input_area;
    app.mouse_regions.breadcrumb_bar = breadcrumb_area;
    app.breadcrumb_segments =
        crate::tui_render::breadcrumb_click_segments(breadcrumb_area, breadcrumb_data);
}

pub fn handle_mouse(app: &mut App, me: MouseEvent, agent: &Arc<TokioMutex<Agent>>) -> bool {
    if app.settings.active || app.pending_confirmation.is_some() {
        return false;
    }

    match me.kind {
        MouseEventKind::Down(MouseButton::Left) => handle_click(app, me.column, me.row, agent),
        MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Moved => {
            handle_scroll_drag(app, me.column, me.row)
        }
        MouseEventKind::Up(MouseButton::Left) => {
            let had = app.scroll_drag_pane.is_some();
            app.scroll_drag_pane = None;
            had
        }
        MouseEventKind::ScrollUp => handle_wheel(app, -3, agent),
        MouseEventKind::ScrollDown => handle_wheel(app, 3, agent),
        _ => false,
    }
}

fn is_pane_scrollbar_col(area: ratatui::layout::Rect, col: u16) -> bool {
    area.width > 0 && col == area.x + area.width - 1
}

fn pane_has_scrollbar(line_count: u16, content_h: u16) -> bool {
    line_count > content_h
}

fn handle_scroll_drag(app: &mut App, col: u16, row: u16) -> bool {
    let Some(pane) = app.scroll_drag_pane else {
        return false;
    };
    let (area, line_count, content_h) = match pane {
        Pane::Left => (
            app.last_left_area,
            app.last_left_line_count,
            app.left_pane_content_height(),
        ),
        Pane::Right => (
            app.last_right_area,
            app.last_right_line_count,
            app.right_pane_content_height(),
        ),
        Pane::Input => return false,
    };
    if !pane_has_scrollbar(line_count, content_h) || !is_pane_scrollbar_col(area, col) {
        app.scroll_drag_pane = None;
        return false;
    }
    app.scroll_pane_from_row(pane, row);
    true
}

fn try_begin_scroll_drag(app: &mut App, col: u16, row: u16) -> bool {
    if pane_has_scrollbar(app.last_left_line_count, app.left_pane_content_height())
        && is_pane_scrollbar_col(app.last_left_area, col)
        && point_in(app.last_left_area, col, row)
    {
        app.scroll_drag_pane = Some(Pane::Left);
        app.scroll_pane_from_row(Pane::Left, row);
        app.needs_redraw = true;
        return true;
    }
    if pane_has_scrollbar(app.last_right_line_count, app.right_pane_content_height())
        && is_pane_scrollbar_col(app.last_right_area, col)
        && point_in(app.last_right_area, col, row)
    {
        app.scroll_drag_pane = Some(Pane::Right);
        app.scroll_pane_from_row(Pane::Right, row);
        app.needs_redraw = true;
        return true;
    }
    false
}

fn handle_wheel(app: &mut App, delta_lines: i16, agent: &Arc<TokioMutex<Agent>>) -> bool {
    if app.desktop.showing_wiki_viewer() {
        if app.wiki_viewer.focus == WikiFocus::Content {
            if delta_lines < 0 {
                app.wiki_viewer.scroll = app.wiki_viewer.scroll.saturating_sub(3);
            } else {
                app.wiki_viewer.scroll = app.wiki_viewer.scroll.saturating_add(3);
            }
        } else {
            wheel_nav_list(
                &mut app.wiki_viewer.selected_nav,
                app.wiki_viewer.nav_items.len(),
                delta_lines,
            );
            app.wiki_viewer.scroll_to_nav_if_current_file();
        }
        app.needs_redraw = true;
        return true;
    }

    if app.desktop.active == ActiveDesktop::Overview {
        return wheel_overview(app, delta_lines, agent);
    }

    if app.desktop.showing_picker() || app.desktop.active == ActiveDesktop::Splash {
        return wheel_picker(app, delta_lines);
    }

    if app.desktop.active == ActiveDesktop::Workspace {
        app.scroll_focused_line(delta_lines);
        app.needs_redraw = true;
        return true;
    }

    false
}

fn wheel_nav_list(selected: &mut usize, len: usize, delta: i16) {
    if len == 0 {
        return;
    }
    if delta < 0 {
        *selected = selected.saturating_sub(1);
    } else {
        *selected = (*selected + 1).min(len - 1);
    }
}

fn wheel_overview(app: &mut App, delta: i16, agent: &Arc<TokioMutex<Agent>>) -> bool {
    match app.view_focus {
        ViewFocus::Picker => {
            wheel_picker_tree(app, delta);
            app.prepare_overview_for_session(agent);
        }
        ViewFocus::Nav => {
            wheel_nav_list(
                &mut app.overview_browser.selected_nav,
                app.overview_browser.nav_items.len(),
                delta,
            );
            app.overview_browser.preview_selected_nav();
        }
        ViewFocus::Content => {
            if app.overview_browser.selected_is_harness() {
                if delta < 0 {
                    app.left_scroll = app.left_scroll.saturating_sub(3);
                } else {
                    app.left_scroll = app.left_scroll.saturating_add(3);
                }
            } else if delta < 0 {
                app.overview_browser.scroll = app.overview_browser.scroll.saturating_sub(3);
            } else {
                app.overview_browser.scroll = app.overview_browser.scroll.saturating_add(3);
            }
        }
    }
    app.needs_redraw = true;
    true
}

fn wheel_picker(app: &mut App, delta: i16) -> bool {
    if app.picker.focus == PickerFocus::Summary {
        if delta < 0 {
            app.picker.summary_scroll = app.picker.summary_scroll.saturating_sub(3);
        } else {
            app.picker.summary_scroll = app.picker.summary_scroll.saturating_add(3);
        }
        app.recompute_active_link();
    } else {
        wheel_picker_tree(app, delta);
    }
    app.needs_redraw = true;
    true
}

fn wheel_picker_tree(app: &mut App, delta: i16) {
    let n = app.picker.picker_items.len();
    if n == 0 {
        return;
    }
    if delta < 0 {
        app.picker.selected_item = app.picker.selected_item.saturating_sub(1);
    } else {
        app.picker.selected_item = (app.picker.selected_item + 1).min(n - 1);
    }
    app.sync_picker_selection();
    app.refresh_picker_summary();
}

fn handle_click(app: &mut App, col: u16, row: u16, agent: &Arc<TokioMutex<Agent>>) -> bool {
    let regions = app.mouse_regions;

    if let Some(target) = crate::tui_render::breadcrumb_target_at(
        regions.breadcrumb_bar,
        &app.breadcrumb_segments,
        col,
        row,
    ) {
        app.navigate_to_breadcrumb(target, agent);
        return true;
    }

    if point_in(regions.input, col, row) {
        app.focused_pane = Pane::Input;
        app.needs_redraw = true;
        return true;
    }

    if app.desktop.showing_wiki_viewer() {
        return click_wiki_viewer(app, col, row, regions);
    }

    match app.desktop.active {
        ActiveDesktop::Splash => click_splash(app, col, row, regions),
        ActiveDesktop::Picker => click_picker_screen(app, col, row, regions, agent),
        ActiveDesktop::Overview => click_overview(app, col, row, regions, agent),
        ActiveDesktop::Workspace => click_workspace(app, col, row),
        _ => false,
    }
}

fn click_splash(app: &mut App, col: u16, row: u16, regions: MouseRegions) -> bool {
    if point_in(regions.splash_magenta, col, row) {
        app.splash_focus = SplashFocus::Magenta;
        app.needs_redraw = true;
        return true;
    }
    if point_in(regions.splash_picker, col, row) {
        app.splash_focus = SplashFocus::Picker;
        app.picker.focus = PickerFocus::Tree;
        if let Some(idx) = list_item_at_row(regions.splash_picker, row, 1, 0, app.picker.picker_items.len())
        {
            app.picker.selected_item = idx;
            app.sync_picker_selection();
            app.refresh_picker_summary();
        }
        app.needs_redraw = true;
        return true;
    }
    false
}

fn click_picker_screen(
    app: &mut App,
    col: u16,
    row: u16,
    regions: MouseRegions,
    _agent: &Arc<TokioMutex<Agent>>,
) -> bool {
    if point_in(regions.picker_tree, col, row) {
        app.picker.focus = PickerFocus::Tree;
        if let Some(idx) =
            list_item_at_row(regions.picker_tree, row, 1, 0, app.picker.picker_items.len())
        {
            app.picker.selected_item = idx;
            app.sync_picker_selection();
            app.refresh_picker_summary();
        }
        app.needs_redraw = true;
        return true;
    }
    if point_in(regions.picker_summary, col, row) {
        app.picker.focus = PickerFocus::Summary;
        if try_click_summary_link(app, regions.picker_summary, row) {
            app.needs_redraw = true;
            return true;
        }
        app.needs_redraw = true;
        return true;
    }
    false
}

fn click_overview(
    app: &mut App,
    col: u16,
    row: u16,
    regions: MouseRegions,
    agent: &Arc<TokioMutex<Agent>>,
) -> bool {
    if point_in(regions.overview_picker, col, row) {
        app.view_focus = ViewFocus::Picker;
        if let Some(idx) =
            list_item_at_row(regions.overview_picker, row, 1, 0, app.picker.picker_items.len())
        {
            app.picker.selected_item = idx;
            app.sync_picker_selection();
            app.refresh_picker_summary();
            app.prepare_overview_for_session(agent);
        }
        app.needs_redraw = true;
        return true;
    }
    if point_in(regions.overview_nav, col, row) {
        app.view_focus = ViewFocus::Nav;
        if let Some(idx) = nav_item_at_row(regions.overview_nav, row, &app.overview_browser) {
            app.overview_browser.selected_nav = idx;
            app.overview_browser.preview_selected_nav();
        }
        app.needs_redraw = true;
        return true;
    }
    if point_in(regions.overview_content, col, row) {
        app.view_focus = ViewFocus::Content;
        if app.overview_browser.selected_is_harness() {
            app.focused_pane = Pane::Left;
        } else {
            app.overview_browser.focus = WikiFocus::Content;
        }
        app.needs_redraw = true;
        return true;
    }
    false
}

fn click_wiki_viewer(app: &mut App, col: u16, row: u16, regions: MouseRegions) -> bool {
    if point_in(regions.wiki_nav, col, row) {
        app.wiki_viewer.focus = WikiFocus::Nav;
        if let Some(idx) = nav_item_at_row(regions.wiki_nav, row, &app.wiki_viewer) {
            app.wiki_viewer.selected_nav = idx;
            app.wiki_viewer.scroll_to_nav_if_current_file();
        }
        app.needs_redraw = true;
        return true;
    }
    if point_in(regions.wiki_content, col, row) {
        app.wiki_viewer.focus = WikiFocus::Content;
        app.needs_redraw = true;
        return true;
    }
    false
}

fn click_workspace(app: &mut App, col: u16, row: u16) -> bool {
    if try_begin_scroll_drag(app, col, row) {
        return true;
    }

    if point_in(app.last_left_area, col, row) {
        app.focused_pane = Pane::Left;
        app.needs_redraw = true;
        return true;
    }
    if point_in(app.last_right_area, col, row) {
        return click_trace_pane(app, col, row);
    }
    app.focused_pane = Pane::Input;
    app.needs_redraw = true;
    true
}

fn click_trace_pane(app: &mut App, _col: u16, row: u16) -> bool {
    app.focused_pane = Pane::Right;
    app.right_follow_output = false;

    let content_y = app.last_right_area.y + 1;
    if row < content_y {
        app.needs_redraw = true;
        return true;
    }

    let vis_idx = app.right_scroll as usize + (row - content_y) as usize;
    let blocks = crate::trace_fold::detect_tool_blocks(&app.trace_lines);
    let visible = crate::trace_fold::compute_visible_lines(
        &app.trace_lines,
        &blocks,
        &app.trace_expanded,
    );

    if let Some(vline) = visible.get(vis_idx) {
        app.trace_cursor_active = true;
        app.trace_cursor = crate::trace_fold::cursor_line_for_visible(&visible, vis_idx);
        if let Some(header_idx) =
            crate::trace_fold::fold_toggle_header(&app.trace_lines, &blocks, vline)
        {
            app.toggle_trace_fold_header(header_idx);
        }
    }

    app.needs_redraw = true;
    true
}

fn nav_item_at_row(
    pane: ratatui::layout::Rect,
    row: u16,
    browser: &crate::wiki_browser::WikiBrowser,
) -> Option<usize> {
    let n = browser.nav_items.len();
    if n == 0 {
        return None;
    }
    let nav_vis = block_inner(pane).height.saturating_sub(1).max(1) as usize;
    let nav_off = list_visible_offset(browser.selected_nav, nav_vis, n);
    list_item_at_row(pane, row, 1, nav_off, n)
}

fn try_click_summary_link(app: &mut App, pane: ratatui::layout::Rect, row: u16) -> bool {
    if app.picker.wiki_links.is_empty() {
        return false;
    }
    let inner = block_inner(pane);
    if row < inner.y || row >= inner.y + inner.height {
        return false;
    }
    let abs_line = app.picker.summary_scroll + (row - inner.y) as usize;
    let wiki_line = abs_line.saturating_sub(app.picker.wiki_content_start);
    if let Some((idx, _)) = app
        .picker
        .wiki_links
        .iter()
        .enumerate()
        .find(|(_, l)| l.line == wiki_line)
    {
        app.picker.active_link_idx = idx;
        app.picker.summary_action = SummaryAction::ViewWiki;
        app.follow_wiki_link_in_summary();
        return true;
    }
    false
}