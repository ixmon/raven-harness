//! Key handling for the full-screen wiki viewer (Screen 3).

use crate::app_state::{App, ViewFocus};
use crate::two_pane_keys::{
    handle_fast_scroll, handle_horizontal_focus, handle_tab, handle_vertical, NavScrollStyle,
    TwoPaneAction,
};
use crate::wiki_browser::WikiFocus;
use crossterm::event::KeyCode;
use raven_tui::agent::Agent;
use std::sync::Arc;
use tokio::sync::Mutex;

pub fn handle_key(app: &mut App, key: KeyCode, agent: &Arc<Mutex<Agent>>) -> bool {
    if !app.desktop.showing_wiki_viewer() {
        return false;
    }

    match key {
        KeyCode::Left | KeyCode::Char('h') => {
            if app.wiki_viewer.focus == WikiFocus::Nav {
                app.desktop.set_overview();
                app.view_focus = ViewFocus::Nav;
                if app.overview_browser.nav_items.is_empty() {
                    let sid = app
                        .picker
                        .sessions
                        .get(app.picker.selected_session)
                        .map(|m| m.session_id.clone());
                    if let Some(sid) = sid {
                        app.overview_browser.open_overview_session(&sid);
                    }
                }
                app.needs_redraw = true;
                true
            } else if handle_horizontal_focus(&mut app.wiki_viewer, key) {
                app.needs_redraw = true;
                true
            } else {
                false
            }
        }
        KeyCode::Right | KeyCode::Char('l') => {
            if app.wiki_viewer.focus == WikiFocus::Content || app.wiki_viewer.selected_is_harness() {
                let sid = app.wiki_viewer.session_id.clone();
                app.desktop.exit_wiki_viewer_to_workspace();
                if !sid.is_empty() {
                    app.activate_session_by_id(&sid, agent);
                }
                app.needs_redraw = true;
                true
            } else if handle_horizontal_focus(&mut app.wiki_viewer, key) {
                app.needs_redraw = true;
                true
            } else {
                false
            }
        }
        KeyCode::Up | KeyCode::Char('k') | KeyCode::Down | KeyCode::Char('j') => {
            match handle_vertical(
                &mut app.wiki_viewer,
                key,
                NavScrollStyle { wrap: true },
            ) {
                TwoPaneAction::NotHandled => false,
                TwoPaneAction::NavChanged => {
                    app.wiki_viewer.scroll_to_nav_if_current_file();
                    app.needs_redraw = true;
                    true
                }
                TwoPaneAction::Handled => {
                    app.needs_redraw = true;
                    true
                }
            }
        }
        KeyCode::PageUp | KeyCode::PageDown | KeyCode::Home | KeyCode::End => {
            match handle_fast_scroll(&mut app.wiki_viewer, key, None) {
                TwoPaneAction::NotHandled => false,
                TwoPaneAction::NavChanged => {
                    app.wiki_viewer.scroll_to_nav_if_current_file();
                    app.needs_redraw = true;
                    true
                }
                TwoPaneAction::Handled => {
                    app.needs_redraw = true;
                    true
                }
            }
        }
        KeyCode::Tab => {
            handle_tab(&mut app.wiki_viewer);
            if app.wiki_viewer.focus == WikiFocus::Nav {
                app.wiki_viewer.scroll_to_nav_if_current_file();
            }
            app.needs_redraw = true;
            true
        }
        KeyCode::Enter => {
            if app.wiki_viewer.focus == WikiFocus::Nav {
                if app.wiki_viewer.selected_is_harness() {
                    let sid = app.wiki_viewer.session_id.clone();
                    app.desktop.exit_wiki_viewer_to_workspace();
                    if !sid.is_empty() {
                        app.activate_session_by_id(&sid, agent);
                    }
                } else if !app.wiki_viewer.nav_items.is_empty() {
                    let idx = app.wiki_viewer.selected_nav;
                    app.wiki_viewer.apply_nav_selection(idx);
                }
                app.needs_redraw = true;
            }
            true
        }
        KeyCode::Backspace => {
            app.wiki_viewer.go_back_to_index();
            app.needs_redraw = true;
            true
        }
        KeyCode::Char('r') | KeyCode::Char('R') => {
            app.wiki_viewer.reload();
            app.needs_redraw = true;
            true
        }
        _ => true,
    }
}