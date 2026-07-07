//! Shared Up/Down/Left/Right/Tab handling for Nav + Content wiki panes.

use crate::wiki_browser::{WikiBrowser, WikiFocus};
use crossterm::event::KeyCode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TwoPaneAction {
    NotHandled,
    Handled,
    /// Nav selection moved; caller may refresh preview or scroll-to-heading.
    NavChanged,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct NavScrollStyle {
    /// When true, Up/Down on nav wraps (full wiki viewer). When false, clamps (overview).
    pub wrap: bool,
}

pub fn handle_vertical(
    browser: &mut WikiBrowser,
    key: KeyCode,
    style: NavScrollStyle,
) -> TwoPaneAction {
    match key {
        KeyCode::Up | KeyCode::Char('k') => {
            if browser.focus == WikiFocus::Nav {
                if browser.nav_items.is_empty() {
                    return TwoPaneAction::Handled;
                }
                let n = browser.nav_items.len();
                if style.wrap {
                    browser.selected_nav = if browser.selected_nav == 0 {
                        n - 1
                    } else {
                        browser.selected_nav - 1
                    };
                } else if browser.selected_nav > 0 {
                    browser.selected_nav -= 1;
                }
                TwoPaneAction::NavChanged
            } else {
                browser.scroll = browser.scroll.saturating_sub(1);
                TwoPaneAction::Handled
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if browser.focus == WikiFocus::Nav {
                if browser.nav_items.is_empty() {
                    return TwoPaneAction::Handled;
                }
                let n = browser.nav_items.len();
                if style.wrap {
                    browser.selected_nav = (browser.selected_nav + 1) % n;
                } else if browser.selected_nav + 1 < n {
                    browser.selected_nav += 1;
                }
                TwoPaneAction::NavChanged
            } else {
                browser.scroll = browser.scroll.saturating_add(1);
                TwoPaneAction::Handled
            }
        }
        _ => TwoPaneAction::NotHandled,
    }
}

/// Left from Content → Nav. Right from Nav → Content. Returns `true` when handled.
pub fn handle_horizontal_focus(browser: &mut WikiBrowser, key: KeyCode) -> bool {
    match key {
        KeyCode::Left | KeyCode::Char('h') => {
            if browser.focus == WikiFocus::Content {
                browser.focus = WikiFocus::Nav;
                true
            } else {
                false
            }
        }
        KeyCode::Right | KeyCode::Char('l') => {
            if browser.focus == WikiFocus::Nav {
                browser.focus = WikiFocus::Content;
                true
            } else {
                false
            }
        }
        _ => false,
    }
}

/// Tab cycles nav items; wraps from last item to Content pane (wiki viewer style).
pub fn handle_tab(browser: &mut WikiBrowser) {
    if browser.focus == WikiFocus::Nav && !browser.nav_items.is_empty() {
        let n = browser.nav_items.len();
        let next = (browser.selected_nav + 1) % n;
        if next == 0 {
            browser.focus = WikiFocus::Content;
        } else {
            browser.selected_nav = next;
        }
    } else {
        browser.focus = WikiFocus::Nav;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wiki_doc::{NavItemKind, WikiNavItem};

    fn sample_browser() -> WikiBrowser {
        let mut b = WikiBrowser::default();
        b.nav_items = vec![
            WikiNavItem {
                label: "a".into(),
                kind: NavItemKind::Header,
                ..Default::default()
            },
            WikiNavItem {
                label: "b".into(),
                kind: NavItemKind::Header,
                ..Default::default()
            },
        ];
        b
    }

    #[test]
    fn vertical_clamp_does_not_wrap() {
        let mut b = sample_browser();
        b.selected_nav = 0;
        assert_eq!(
            handle_vertical(&mut b, KeyCode::Up, NavScrollStyle { wrap: false }),
            TwoPaneAction::NavChanged
        );
        assert_eq!(b.selected_nav, 0);
    }

    #[test]
    fn vertical_wrap_cycles() {
        let mut b = sample_browser();
        b.selected_nav = 0;
        handle_vertical(&mut b, KeyCode::Up, NavScrollStyle { wrap: true });
        assert_eq!(b.selected_nav, 1);
    }
}