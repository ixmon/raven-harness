//! Session/workspace picker and overview screen key handling.

use crate::app_state::{App, SplashFocus, SummaryAction, ViewFocus};
use crate::wiki_browser::WikiFocus;
use crossterm::event::KeyCode;
use raven_tui::agent::Agent;
use std::sync::Arc;
use tokio::sync::Mutex;

fn overview_browser_preview(app: &mut App) {
    app.overview_browser.preview_selected_nav();
    app.needs_redraw = true;
}

/// Leave the overview content preview for the full harness or wiki screen.
fn advance_from_overview_content(app: &mut App, agent: &Arc<Mutex<Agent>>) {
    if app.overview_browser.selected_is_harness() {
        app.reset_left_pane_for_harness();
        app.activate_overview_harness_session(agent);
    } else {
        app.enter_wiki_from_overview_content();
    }
}

pub fn handle_key(app: &mut App, key: KeyCode, agent: &Arc<Mutex<Agent>>) -> bool {
    if !is_picker_key_active(app) {
        return false;
    }
    if handle_splash_magenta(app, key) {
        return true;
    }
    if handle_picker_trust_confirm(app, key, agent) {
        return true;
    }
    if app.desktop.active == crate::desktop::ActiveDesktop::Overview {
        return handle_overview(app, key, agent);
    }
    handle_screen(app, key, agent)
}

fn is_picker_key_active(app: &App) -> bool {
    app.desktop.showing_picker()
        || app.desktop.active == crate::desktop::ActiveDesktop::Splash
        || app.desktop.active == crate::desktop::ActiveDesktop::Overview
}

fn remove_session_dir(session_id: &str) -> std::io::Result<()> {
    let dir = raven_tui::session::session_dir(session_id);
    if dir.exists() {
        std::fs::remove_dir_all(dir)
    } else {
        Ok(())
    }
}

fn handle_splash_magenta(app: &mut App, key: KeyCode) -> bool {
    use crate::app_state::PickerFocus;
        if app.desktop.active != crate::desktop::ActiveDesktop::Splash
            || app.splash_focus == SplashFocus::Picker
        {
            return false;
        }
        match key {
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Tab => {
                app.splash_focus = SplashFocus::Picker;
                app.picker.focus = PickerFocus::Tree;
                app.needs_redraw = true;
                true
            }
            KeyCode::Left | KeyCode::Char('h') | KeyCode::Esc => {
                app.needs_redraw = true;
                true
            }
            _ => true,
        }
    }

fn handle_picker_trust_confirm(
    app: &mut App,
    key: KeyCode,
    agent: &Arc<Mutex<Agent>>,
) -> bool {
        let Some(p) = app.picker.confirm_trust_path.clone() else {
            return false;
        };
        match key {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                app.picker.confirm_trust_path = None;
                app.clear_input();
                match raven_tui::session::Session::init(&p) {
                    Ok(mut new_sess) => {
                        new_sess.meta.trusted = true;
                        let _ = new_sess.save_meta();
                        let _ = raven_tui::session::ensure_repo_cache(&mut new_sess);
                        if let Ok(mut ag) = agent.try_lock() {
                            *ag.session_mut() = Some(new_sess);
                        }
                        app.refresh_picker();
                        app.needs_redraw = true;
                    }
                    Err(e) => {
                        app.left_committed.push(format!("Error: {}", e));
                        app.needs_redraw = true;
                    }
                }
                true
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                app.picker.confirm_trust_path = None;
                app.clear_input();
                app.needs_redraw = true;
                true
            }
            _ => true,
        }
    }

fn handle_overview(app: &mut App, key: KeyCode, agent: &Arc<Mutex<Agent>>) -> bool {
        use crate::app_state::SplashFocus;
        match key {
            KeyCode::Up | KeyCode::Char('k') => {
                match app.view_focus {
                    ViewFocus::Picker => {
                        if app.picker.selected_item > 0 {
                            app.picker.selected_item -= 1;
                            app.sync_picker_selection();
                            app.refresh_picker_summary();
                            app.prepare_overview_for_session(agent);
                        }
                    }
                    ViewFocus::Nav => {
                        if app.overview_browser.selected_nav > 0 {
                            app.overview_browser.selected_nav -= 1;
                        }
                        overview_browser_preview(app);
                    }
                    ViewFocus::Content => {
                        if app.overview_browser.selected_is_harness() {
                            app.left_scroll = app.left_scroll.saturating_sub(1);
                        } else {
                            app.overview_browser.scroll = app.overview_browser.scroll.saturating_sub(1);
                        }
                    }
                }
                app.needs_redraw = true;
                true
            }
            KeyCode::Down | KeyCode::Char('j') => {
                match app.view_focus {
                    ViewFocus::Picker => {
                        if app.picker.selected_item + 1 < app.picker.picker_items.len() {
                            app.picker.selected_item += 1;
                            app.sync_picker_selection();
                            app.refresh_picker_summary();
                            app.prepare_overview_for_session(agent);
                        }
                    }
                    ViewFocus::Nav => {
                        if app.overview_browser.selected_nav + 1 < app.overview_browser.nav_items.len() {
                            app.overview_browser.selected_nav += 1;
                        }
                        overview_browser_preview(app);
                    }
                    ViewFocus::Content => {
                        if app.overview_browser.selected_is_harness() {
                            app.left_scroll = app.left_scroll.saturating_add(1);
                        } else {
                            app.overview_browser.scroll = app.overview_browser.scroll.saturating_add(1);
                        }
                    }
                }
                app.needs_redraw = true;
                true
            }
            KeyCode::Left | KeyCode::Char('h') => {
                match app.view_focus {
                    ViewFocus::Picker => {
                        app.desktop.exit_overview_to_splash();
                        app.splash_focus = SplashFocus::Picker;
                    }
                    ViewFocus::Nav => {
                        app.view_focus = ViewFocus::Picker;
                    }
                    ViewFocus::Content => {
                        app.view_focus = ViewFocus::Nav;
                    }
                }
                app.needs_redraw = true;
                true
            }
            KeyCode::Right | KeyCode::Char('l') => {
                match app.view_focus {
                    ViewFocus::Picker => {
                        app.view_focus = ViewFocus::Nav;
                    }
                    ViewFocus::Nav => {
                        app.focus_overview_to_content();
                    }
                    ViewFocus::Content => {
                        advance_from_overview_content(app, agent);
                    }
                }
                app.needs_redraw = true;
                true
            }
            KeyCode::Enter => {
                if app.view_focus == ViewFocus::Content {
                    if app.overview_browser.selected_is_harness() {
                        app.reset_left_pane_for_harness();
                        app.activate_overview_harness_session(agent);
                    } else {
                        app.enter_wiki_viewer();
                    }
                } else if app.view_focus == ViewFocus::Nav {
                    app.view_focus = ViewFocus::Content;
                } else {
                    let item = app.picker.picker_items.get(app.picker.selected_item).cloned();
                    app.sync_picker_selection();
                    if let Some(item) = item {
                        if item.session_id.is_some() || item.depth == 1 {
                            app.activate_selected_session(agent);
                        }
                    }
                }
                app.needs_redraw = true;
                true
            }
            KeyCode::Tab => {
                match app.view_focus {
                    ViewFocus::Picker => {
                        app.view_focus = ViewFocus::Nav;
                    }
                    ViewFocus::Nav => {
                        app.focus_overview_to_content();
                    }
                    ViewFocus::Content => {
                        advance_from_overview_content(app, agent);
                    }
                }
                app.needs_redraw = true;
                true
            }
            _ => false,
        }
    }

fn handle_screen(app: &mut App, key: KeyCode, agent: &Arc<Mutex<Agent>>) -> bool {
        use crate::app_state::{PickerFocus, SplashFocus};
        match key {
            KeyCode::Up | KeyCode::Char('k') => {
                match app.picker.focus {
                    PickerFocus::Tree => {
                        if app.picker.selected_item > 0 {
                            app.picker.selected_item -= 1;
                            app.sync_picker_selection();
                            app.refresh_picker_summary();
                        }
                    }
                    PickerFocus::Summary => {
                        app.picker.summary_scroll = app.picker.summary_scroll.saturating_sub(1);
                        app.recompute_active_link();
                        app.needs_redraw = true;
                    }
                }
                app.needs_redraw = true;
                true
            }
            KeyCode::Down | KeyCode::Char('j') => {
                match app.picker.focus {
                    PickerFocus::Tree => {
                        if app.picker.selected_item + 1 < app.picker.picker_items.len() {
                            app.picker.selected_item += 1;
                            app.sync_picker_selection();
                            app.refresh_picker_summary();
                        }
                    }
                    PickerFocus::Summary => {
                        app.picker.summary_scroll = app.picker.summary_scroll.saturating_add(1);
                        app.recompute_active_link();
                        app.needs_redraw = true;
                    }
                }
                app.needs_redraw = true;
                true
            }
            KeyCode::Left | KeyCode::Char('h') => {
                if app.desktop.active == crate::desktop::ActiveDesktop::Splash
                    && app.splash_focus == SplashFocus::Picker
                {
                    app.splash_focus = SplashFocus::Magenta;
                    app.needs_redraw = true;
                    return true;
                }
                // on magenta, left may exit to prior (workspace) via caller, fallthrough ok
                match app.picker.focus {
                    PickerFocus::Summary => {
                        app.picker.focus = PickerFocus::Tree;
                    }
                    _ => {
                        app.exit_picker_to_main();
                        app.splash_focus = SplashFocus::Magenta;
                    }
                }
                app.needs_redraw = true;
                true
            }
            KeyCode::Right | KeyCode::Char('l') => {
                match app.picker.focus {
                    PickerFocus::Tree => {
                        if app.desktop.active == crate::desktop::ActiveDesktop::Splash {
                            // Right from picker in Screen 1 -> Screen 2 (picker + nav + content)
                            app.prepare_overview_for_session(agent);
                            app.desktop.set_overview();
                            app.view_focus = ViewFocus::Picker;
                            app.wiki_viewer.session_id = app
                                .picker
                                .sessions
                                .get(app.picker.selected_session)
                                .map(|m| m.session_id.clone())
                                .unwrap_or_default();
                            app.wiki_viewer.focus = WikiFocus::Nav;
                            app.needs_redraw = true;
                            return true;
                        }
                        app.picker.focus = PickerFocus::Summary;
                        app.picker.summary_scroll = 0;
                        app.refresh_picker_summary();
                        app.recompute_active_link();
                    }
                    PickerFocus::Summary => {
                        // Right from Summary -> full wiki viewer for selected session
                        app.enter_wiki_viewer();
                    }
                }
                app.needs_redraw = true;
                true
            }
            KeyCode::Enter => {
                match app.picker.focus {
                    PickerFocus::Tree => {
                        let item = app.picker.picker_items.get(app.picker.selected_item).cloned();
                        app.sync_picker_selection();
                        if let Some(item) = item {
                            if item.session_id.is_some() || item.depth == 1 {
                                app.activate_selected_session(agent);
                            } else {
                                // workspace header row -> focus its (first/newest) session summary
                                app.picker.focus = PickerFocus::Summary;
                                app.refresh_picker_summary();
                            }
                        }
                    }
                    PickerFocus::Summary => {
                        let n_links = app.picker.wiki_links.len();
                        let idx = app.picker.active_link_idx;
                        if app.picker.show_wiki_in_summary && idx < n_links && n_links > 0 {
                            // Active item is a wiki link — follow it
                            app.follow_wiki_link_in_summary();
                        } else if idx == n_links {
                            // Wiki button -> full dedicated wiki viewer screen
                            app.enter_wiki_viewer();
                        } else if idx == n_links + 1 {
                            // Launch button
                            app.activate_selected_session(agent);
                        } else {
                            // No links, default to wiki viewer
                            app.enter_wiki_viewer();
                        }
                    }
                }
                app.needs_redraw = true;
                true
            }
            KeyCode::Tab => {
                if app.desktop.active == crate::desktop::ActiveDesktop::Splash
                    && app.splash_focus == SplashFocus::Picker
                {
                    // Tab while picker highlighted on splash: slide to 3-col
                    app.prepare_overview_for_session(agent);
                    app.desktop.set_overview();
                    app.wiki_viewer.focus = WikiFocus::Nav;
                    app.needs_redraw = true;
                    return true;
                }
                // Cycle through: wiki links → Wiki button → Launch button → back to links
                if app.picker.focus == PickerFocus::Summary {
                    let n_links = app.picker.wiki_links.len();
                    let total = n_links + 2; // +2 for Wiki and Launch buttons
                    app.picker.active_link_idx = (app.picker.active_link_idx + 1) % total;
                    let idx = app.picker.active_link_idx;

                    if idx < n_links {
                        // On a link — update summary_action to ViewWiki and scroll to it
                        app.picker.summary_action = SummaryAction::ViewWiki;
                        if let Some(link) = app.picker.wiki_links.get(idx) {
                            let visible_h = app.picker.last_summary_height.max(5) as usize;
                            if link.line < app.picker.summary_scroll
                                || link.line >= app.picker.summary_scroll + visible_h
                            {
                                app.picker.summary_scroll = link.line.saturating_sub(2);
                            }
                        }
                    } else if idx == n_links {
                        // Wiki button
                        app.picker.summary_action = SummaryAction::ViewWiki;
                    } else {
                        // Launch button
                        app.picker.summary_action = SummaryAction::Launch;
                    }
                    app.needs_redraw = true;
                }
                true
            }
            KeyCode::Backspace => {
                // Navigate back to index.md in wiki mode
                if app.picker.focus == PickerFocus::Summary
                    && app.picker.show_wiki_in_summary
                    && app.picker.current_wiki_file != "index.md"
                {
                    app.picker.current_wiki_file = "index.md".to_string();
                    app.picker.summary_scroll = 0;
                    app.refresh_picker_summary();
                    app.recompute_active_link();
                    app.needs_redraw = true;
                }
                true
            }
            KeyCode::Char('a') | KeyCode::Char('A') => {
                app.picker.adding_workspace = true;
                app.input.clear();
                app.cursor_pos = 0;
                app.needs_redraw = true;
                true
            }
            KeyCode::Char('n') | KeyCode::Char('N') => {
                // New session for current ws — the tree now owns selection (sessions live under ws rows)
                if let Some(ws) = app.picker.workspaces.get(app.picker.selected_workspace) {
                    let ws_path = ws.path.clone();
                    let name = format!("new-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs());
                    if let Ok(new_sess) = raven_tui::session::Session::init_named(&ws_path, &name) {
                        let new_id = new_sess.id.clone();
                        app.refresh_picker();
                        // Prefer stable session id to pick the correct tree row (newest first after sort)
                        if let Some(pos) = app.picker.picker_items.iter().position(|it| {
                            it.session_id.as_ref() == Some(&new_id)
                        }) {
                            app.picker.selected_item = pos;
                            app.sync_picker_selection();
                            app.refresh_picker_summary();
                        } else if let Some(pos) = app.picker.picker_items.iter().position(|it| {
                            it.workspace_path == ws_path && it.depth == 1
                        }) {
                            // Fallback: first session row under this ws (newest)
                            app.picker.selected_item = pos;
                            app.sync_picker_selection();
                            app.refresh_picker_summary();
                        }
                    }
                }
                app.needs_redraw = true;
                true
            }
            KeyCode::Char('d') | KeyCode::Char('D') => {
                // Delete current focus item (tree now combines ws + sessions)
                if app.picker.focus == PickerFocus::Tree {
                    if let Some(item) = app.picker.picker_items.get(app.picker.selected_item) {
                        if let Some(ref sid) = item.session_id {
                            let _ = remove_session_dir(sid);
                        } else {
                            // ws header: delete all its sessions (use stable path)
                            if let Ok(ses) = raven_tui::session::list_sessions_for(&item.workspace_path) {
                                for s in ses {
                                    let _ = remove_session_dir(&s.session_id);
                                }
                            }
                        }
                        app.refresh_picker();
                    }
                }
                app.needs_redraw = true;
                true
            }
            KeyCode::Char('w') | KeyCode::Char('W') => {
                if app.picker.focus == PickerFocus::Summary {
                    app.picker.summary_action = SummaryAction::ViewWiki;
                    app.picker.show_wiki_in_summary = !app.picker.show_wiki_in_summary;
                    if app.picker.show_wiki_in_summary {
                        app.refresh_picker_summary();
                        app.recompute_active_link();
                    }
                    app.needs_redraw = true;
                    return true;
                } else if app.picker.focus == PickerFocus::Tree {
                    // From tree, if on a session row, dump its wiki to main left pane (like old behavior)
                    if let Some(item) = app.picker.picker_items.get(app.picker.selected_item) {
                        if let Some(ref sid) = item.session_id {
                            let wiki_file = if app.picker.current_wiki_file.is_empty() {
                                "index.md".to_string()
                            } else {
                                app.picker.current_wiki_file.clone()
                            };
                            let content = raven_tui::session::read_session_wiki_file(sid, &wiki_file)
                                .unwrap_or_else(|| format!("(no {wiki_file} yet — agent can create one with write_wiki)"));
                            app.left_committed.push(format!("=== Wiki: {} ===\n{}", wiki_file, content));
                            app.left_follow_output = true;
                            app.left_scroll = 10_000;
                            app.needs_redraw = true;
                            return true;
                        }
                    }
                }
                true
            }
            KeyCode::PageUp => {
                if app.picker.focus == PickerFocus::Tree || app.picker.focus == PickerFocus::Summary {
                    app.picker.summary_scroll = app.picker.summary_scroll.saturating_sub(12);
                    app.recompute_active_link();
                    app.needs_redraw = true;
                    return true;
                }
                false
            }
            KeyCode::PageDown => {
                if app.picker.focus == PickerFocus::Tree || app.picker.focus == PickerFocus::Summary {
                    app.picker.summary_scroll = app.picker.summary_scroll.saturating_add(12);
                    app.recompute_active_link();
                    app.needs_redraw = true;
                    return true;
                }
                false
            }
            _ => false,
        }
    }


