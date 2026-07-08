//! Unified wiki browsing state (full viewer + overview nav pane).

use crate::wiki_doc::{self, NavItemKind, WikiNavItem};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WikiFocus {
    #[default]
    Nav,
    Content,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WikiNavKind {
    #[default]
    Viewer,
    Overview,
}

/// Updated each frame during wiki/overview render for page-scroll key handling.
#[derive(Debug, Default, Clone, Copy)]
pub struct WikiScrollMetrics {
    pub content_visible: u16,
    pub nav_visible: u16,
    pub content_lines: usize,
}

#[derive(Debug, Default)]
pub struct WikiBrowser {
    pub session_id: String,
    pub current_file: String,
    pub content: String,
    pub scroll: usize,
    pub nav_items: Vec<WikiNavItem>,
    pub selected_nav: usize,
    pub focus: WikiFocus,
    /// File inventory for the full wiki viewer screen.
    pub files: Vec<String>,
    pub scroll_metrics: WikiScrollMetrics,
    nav_kind: WikiNavKind,
}

impl WikiBrowser {
    pub fn selected_is_harness(&self) -> bool {
        wiki_doc::nav_is_harness(&self.nav_items, self.selected_nav)
    }

    /// Initialize full wiki viewer for a session (starts at index.md).
    pub fn open_viewer_session(&mut self, session_id: &str) {
        self.session_id = session_id.to_string();
        self.current_file = "index.md".to_string();
        self.focus = WikiFocus::Nav;
        self.scroll = 0;
        self.selected_nav = 0;
        self.files = raven_tui::session::collect_session_wiki_md_files(session_id);
        self.nav_kind = WikiNavKind::Viewer;
        self.load_current_file();
        if let Some(pos) = self
            .nav_items
            .iter()
            .position(|it| it.target_file == self.current_file)
        {
            self.selected_nav = pos;
        }
    }

    /// Build overview browser nav from session index (stable Harness + Wiki tree).
    pub fn open_overview_session(&mut self, session_id: &str) {
        self.session_id = session_id.to_string();
        self.nav_kind = WikiNavKind::Overview;
        self.selected_nav = 0;
        self.scroll = 0;
        let index_content =
            raven_tui::session::read_session_wiki_file(session_id, "index.md").unwrap_or_default();
        self.nav_items = wiki_doc::build_browser_overview_nav(&index_content);
        self.content = index_content;
    }

    pub fn load_current_file(&mut self) {
        if self.session_id.is_empty() {
            return;
        }
        let clean = wiki_doc::normalize_wiki_path(&self.current_file);
        self.content = raven_tui::session::read_session_wiki_file(&self.session_id, &clean)
            .unwrap_or_else(|| format!("(could not read {})", clean));
        self.current_file = clean;
        self.rebuild_nav();
    }

    pub fn reload(&mut self) {
        self.load_current_file();
    }

    fn rebuild_nav(&mut self) {
        if self.nav_kind != WikiNavKind::Viewer {
            return;
        }
        self.nav_items = wiki_doc::build_viewer_nav(&self.content, &self.current_file);
        if self.selected_nav >= self.nav_items.len() {
            self.selected_nav = 0;
        }
    }

    /// Overview: update content preview when nav selection changes.
    pub fn preview_selected_nav(&mut self) {
        if self.nav_kind != WikiNavKind::Overview {
            return;
        }
        if let Some(item) = self.nav_items.get(self.selected_nav) {
            if item.kind == NavItemKind::Harness {
                self.content.clear();
            } else {
                self.content = raven_tui::session::read_session_wiki_file(
                    &self.session_id,
                    &item.target_file,
                )
                .unwrap_or_else(|| format!("(could not read {})", item.target_file));
            }
        }
        self.scroll = 0;
    }

    /// Viewer: apply nav item (may load a different file).
    pub fn apply_nav_selection(&mut self, idx: usize) {
        if idx >= self.nav_items.len() {
            return;
        }
        let item = self.nav_items[idx].clone();
        self.selected_nav = idx;
        if item.kind == NavItemKind::Harness {
            return;
        }
        let clean_target = wiki_doc::normalize_wiki_path(&item.target_file);
        let clean_cur = wiki_doc::normalize_wiki_path(&self.current_file);

        let is_cross_file = clean_target != clean_cur
            && matches!(item.kind, NavItemKind::Back | NavItemKind::Link);

        if is_cross_file {
            self.current_file = clean_target;
            self.load_current_file();
            if let Some(first) = self
                .nav_items
                .iter()
                .position(|it| it.kind == NavItemKind::Header)
            {
                self.selected_nav = first;
            } else {
                self.selected_nav = 0;
            }
            self.scroll = 0;
        } else {
            self.scroll = item.scroll_to;
        }
    }

    /// If the selected nav target is in the current file, scroll content to it.
    pub fn scroll_to_nav_if_current_file(&mut self) {
        if let Some(item) = self.nav_items.get(self.selected_nav) {
            let clean_target = wiki_doc::normalize_wiki_path(&item.target_file);
            let clean_cur = wiki_doc::normalize_wiki_path(&self.current_file);
            if clean_target == clean_cur {
                self.scroll = item.scroll_to;
            }
        }
    }

    pub fn go_back_to_index(&mut self) {
        if self.current_file == "index.md" {
            return;
        }
        self.current_file = "index.md".to_string();
        self.scroll = 0;
        self.load_current_file();
        if let Some(pos) = self
            .nav_items
            .iter()
            .position(|it| it.target_file == "index.md")
        {
            self.selected_nav = pos;
        } else {
            self.selected_nav = 0;
        }
    }

    fn content_page_lines(&self) -> usize {
        self.scroll_metrics.content_visible.max(5) as usize
    }

    fn nav_page_items(&self) -> usize {
        self.scroll_metrics.nav_visible.max(1) as usize
    }

    pub fn scroll_content_by_page(&mut self, delta: i16) {
        let vis = self.content_page_lines();
        let max = self
            .scroll_metrics
            .content_lines
            .saturating_sub(vis);
        if delta < 0 {
            self.scroll = self.scroll.saturating_sub(vis);
        } else {
            self.scroll = (self.scroll + vis).min(max);
        }
    }

    pub fn scroll_content_home(&mut self) {
        self.scroll = 0;
    }

    pub fn scroll_content_end(&mut self) {
        let vis = self.content_page_lines();
        self.scroll = self
            .scroll_metrics
            .content_lines
            .saturating_sub(vis);
    }

    pub fn scroll_nav_by_page(&mut self, delta: i16) {
        if self.nav_items.is_empty() {
            return;
        }
        let page = self.nav_page_items();
        let n = self.nav_items.len();
        if delta < 0 {
            self.selected_nav = self.selected_nav.saturating_sub(page);
        } else {
            self.selected_nav = (self.selected_nav + page).min(n - 1);
        }
    }

    pub fn scroll_nav_home(&mut self) {
        if !self.nav_items.is_empty() {
            self.selected_nav = 0;
        }
    }

    pub fn scroll_nav_end(&mut self) {
        if !self.nav_items.is_empty() {
            self.selected_nav = self.nav_items.len() - 1;
        }
    }

    /// Seed viewer file before opening the full wiki viewer from overview.
    pub fn seed_viewer_file(&mut self, session_id: &str, target_file: &str) {
        self.session_id = session_id.to_string();
        self.current_file = target_file.to_string();
        self.nav_kind = WikiNavKind::Viewer;
        self.load_current_file();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_page_scroll_clamps_to_end() {
        let mut b = WikiBrowser::default();
        b.scroll_metrics = WikiScrollMetrics {
            content_visible: 10,
            nav_visible: 5,
            content_lines: 25,
        };
        b.scroll = 0;
        b.scroll_content_by_page(1);
        assert_eq!(b.scroll, 10);
        b.scroll_content_end();
        assert_eq!(b.scroll, 15);
        b.scroll_content_home();
        assert_eq!(b.scroll, 0);
    }

    #[test]
    fn nav_page_scroll_moves_selection() {
        let mut b = WikiBrowser::default();
        b.nav_items = vec![
            WikiNavItem::default(),
            WikiNavItem::default(),
            WikiNavItem::default(),
            WikiNavItem::default(),
        ];
        b.scroll_metrics.nav_visible = 2;
        b.selected_nav = 0;
        b.scroll_nav_by_page(1);
        assert_eq!(b.selected_nav, 2);
        b.scroll_nav_end();
        assert_eq!(b.selected_nav, 3);
        b.scroll_nav_home();
        assert_eq!(b.selected_nav, 0);
    }

    #[test]
    fn harness_detection_uses_nav_items() {
        let mut b = WikiBrowser::default();
        b.nav_items = vec![crate::wiki_doc::WikiNavItem {
            kind: crate::wiki_doc::NavItemKind::Harness,
            ..Default::default()
        }];
        assert!(b.selected_is_harness());
    }
}