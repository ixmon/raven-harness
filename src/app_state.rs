//! App state and core types for the TUI.

use std::sync::Arc;

use std::sync::atomic::{AtomicBool, Ordering};

use crate::desktop::DesktopState;
use crate::key_edit::EditAction;
use crate::search::SearchState;
use crate::settings_modal::SettingsModal;
use crossterm::event::KeyCode;
use raven_tui::agent::Agent;
use raven_tui::session::{SessionMeta, WorkspaceEntry};

pub use crate::plan_state::PlanState;
pub use crate::wiki_doc::{
    nav_is_harness as browser_nav_is_harness, NavItemKind, WikiLink, WikiNavItem,
};

#[cfg(feature = "clipboard")]
#[allow(dead_code)]
fn try_copy_to_clipboard(text: &str) {
    if let Ok(mut clipboard) = arboard::Clipboard::new() {
        let _ = clipboard.set_text(text.to_string());
    }
}

#[cfg(feature = "clipboard")]
fn try_read_clipboard() -> Option<String> {
    arboard::Clipboard::new()
        .ok()
        .and_then(|mut clipboard| clipboard.get_text().ok())
        .filter(|s| !s.is_empty())
}

#[cfg(not(feature = "clipboard"))]
fn try_read_clipboard() -> Option<String> {
    None
}

/// Pane selection for UI navigation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Pane {
    Left,
    Right,
    Input,
}

/// Focus within the session/workspace picker screen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PickerFocus {
    #[default]
    Tree,
    Summary,
}

/// Focus state on the initial splash screen (magenta pane vs workspace picker).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SplashFocus {
    #[default]
    Magenta,
    Picker,
}

/// Focus for the 3-pane "picker + nav + content" screen (Screen 2).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ViewFocus {
    #[default]
    Picker,
    Nav,
    Content,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SummaryAction {
    #[default]
    ViewWiki,
    Launch,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WikiFocus {
    #[default]
    Nav,
    Content,
}

#[derive(Clone, Debug)]
pub struct PickerItem {
    pub depth: usize,
    pub label: String,
    // Transient index into current self.workspaces / self.sessions at build time (for compat).
    pub workspace_idx: usize,
    pub session_idx: Option<usize>, // None = workspace row
    // Stable keys for reliable re-selection after refresh (new 'n', deletes, etc.)
    pub workspace_path: std::path::PathBuf,
    pub session_id: Option<String>,
}

#[derive(Debug, Default)]
pub struct WikiViewerState {
    pub session_id: String,
    pub current_file: String,
    pub scroll: usize,
    pub files: Vec<String>,
    pub focus: WikiFocus,
    pub content: String,
    // Navigational elements (files + parsed links/headings from current wiki md)
    pub nav_items: Vec<WikiNavItem>,
    pub selected_nav: usize,
}

impl WikiViewerState {
    pub fn selected_is_harness(&self) -> bool {
        crate::wiki_doc::nav_is_harness(&self.nav_items, self.selected_nav)
    }
}

#[derive(Debug, Default)]
pub struct PickerState {
    pub workspaces: Vec<WorkspaceEntry>,
    pub selected_workspace: usize,
    pub sessions: Vec<SessionMeta>, // for currently selected workspace
    pub selected_session: usize,
    pub focus: PickerFocus,
    pub loaded: bool,
    pub summary: String,
    pub summary_scroll: usize,
    // Tree view: combined workspaces + indented sessions (replaces separate sessions pane)
    pub picker_items: Vec<PickerItem>,
    pub selected_item: usize,
    // Current file shown in the summary/wiki column (relative to wiki/ dir)
    pub current_wiki_file: String,
    pub current_wiki_content: String,
    pub wiki_links: Vec<WikiLink>,
    pub active_link_idx: usize,
    pub wiki_content_start: usize, // line index in summary where wiki content (after --- header) starts
    pub last_summary_height: u16, // for better visible window in active link calc
    pub show_wiki_in_summary: bool, // hide wiki preview behind "Wiki" link initially
    pub summary_action: SummaryAction,
    // For "Add workspace" flow
    pub adding_workspace: bool,
    pub confirm_trust_path: Option<std::path::PathBuf>,
}



/// Holds all the mutable UI state for the TUI application.
pub struct App {
    // Conversation / output
    pub left_committed: Vec<String>,
    pub current_response: String,

    // Right pane
    pub trace_lines: Vec<String>,
    pub current_thinking: String,

    // Input
    pub input: String,
    pub cursor_pos: usize, // byte offset into `input`, always on a char boundary

    // Navigation / scroll
    pub left_scroll: u16,
    pub right_scroll: u16,
    pub left_follow_output: bool,
    pub right_follow_output: bool,
    pub focused_pane: Pane,
    pub scroll_flash_timer: u8,

    // Agent turn queued from async plan-loop proceed classification.
    pub deferred_agent_prompt: Option<String>,

    // Processing state
    pub is_processing: bool,
    pub spinner_tick: usize,
    pub tool_calls_this_turn: usize,
    pub turn_rounds: usize,
    pub ctx_used_tokens: u32,
    #[allow(dead_code)]
    pub processing_start: Option<std::time::Instant>,
    pub tokens_processed: u32,
    pub tps: f64,
    // API-reported usage for TPS calculation
    pub api_prompt_tokens: Option<u32>,
    pub api_completion_tokens: Option<u32>,
    pub api_total_tokens: Option<u32>,
    pub api_tps: f64,

    // Accurate generation time for TPS: only counts time between consecutive
    // output tokens (Token/Thinking). This excludes tool execution, approvals,
    // waiting for user, multi-round gaps, etc.
    pub generation_active_time: f64,
    pub last_token_time: Option<std::time::Instant>,

    // For Super Judge inactivity trigger in work mode
    #[allow(dead_code)]
    pub last_turn_end: Option<std::time::Instant>,

    // Modal confirmations (tool approval, plan entry, …)
    pub pending_confirmation: Option<crate::confirmation_dialog::ConfirmationDialog>,
    pub needs_redraw: bool,

    // Mode menu
    pub mode_menu_active: bool,
    pub selected_mode_idx: usize,
    pub approval_modes: [&'static str; 4],
    /// Live approval mode for in-flight agent turns (TuiObserver reads this).
    pub live_exec_mode: std::sync::Arc<std::sync::Mutex<raven_tui::session::ExecApprovalMode>>,

    // Run mode submenu for /run-mode (talk, think, ...)
    pub agent_mode_menu_active: bool,
    pub selected_agent_mode_idx: usize,
    pub agent_modes: [&'static str; 6],

    // Slash menu
    pub slash_commands: Vec<crate::input_dispatch::SlashCommand>,
    pub slash_selected: usize,

    // Display state (updated on endpoint switch)
    pub display_model: String,
    pub display_budget: raven_tui::config::ContextBudget,
    pub balance_label: String,

    // Settings modal (extracted module)
    pub settings: SettingsModal,

    // Last draw layout (for scroll clamping in keys)
    pub last_left_line_count: u16,
    pub last_right_line_count: u16,
    pub last_left_area: ratatui::layout::Rect,
    pub last_right_area: ratatui::layout::Rect,

    // Input history for up/down recall (glm.md UX)
    #[allow(dead_code)]
    pub input_history: Vec<String>,
    pub history_index: Option<usize>,

    // Conversation / trace search
    pub search: SearchState,
    pub search_mode: bool,

    // Cached status-bar labels to avoid flicker when agent lock is contended (glm.md)
    pub cached_mode_label: String,
    pub cached_goal_text: String,
    pub cached_agent_mode: String,

    // Multi-desktop: workspace ↔ splash (left/right arrow slide)
    pub desktop: DesktopState,
    pub raven_art: String,

    /// Focus on the splash (first) screen: magenta pane (default) or workspace picker.
    pub splash_focus: SplashFocus,

    /// Focus for the picker+nav+content view (Screen 2 after sliding from splash).
    pub view_focus: ViewFocus,

    // For Screen 2 browser nav (separate from wiki_viewer to keep stable tree)
    pub browser_nav_items: Vec<WikiNavItem>,
    pub browser_selected_nav: usize,
    pub browser_wiki_content: String,
    pub browser_wiki_scroll: usize,

    // Session / workspace picker (new screen to the right of splash)
    pub picker: PickerState,

    // Full wiki viewer screen
    pub wiki_viewer: WikiViewerState,

    // Plan Mode state (new pane + run mode "plan")
    pub plan: PlanState,
}

#[allow(dead_code)]
impl App {
    /// Create a new App instance with default values.
    pub fn new(config: &raven_tui::config::Config) -> Self {
        let banner = format!(
            "Raven Hotel - Agent Harness\n\n\
             Endpoint: {}\n\
             Model:    {}\n\
             Workspace: {}\n\n\
             Type / in the input for commands (help, clear, reset, status...).\n\
             Use Ctrl-C to quit.",
            config.base_url,
            config.model,
            config.workspace.display()
        );
        Self {
            left_committed: vec![banner],
            current_response: String::new(),
            trace_lines: vec![],
            current_thinking: String::new(),
            input: String::new(),
            cursor_pos: 0,
            left_scroll: 0,
            right_scroll: 0,
            left_follow_output: true,
            right_follow_output: true,
            focused_pane: Pane::Left,
            scroll_flash_timer: 0,
            deferred_agent_prompt: None,
            is_processing: false,
            spinner_tick: 0,
            tool_calls_this_turn: 0,
            turn_rounds: 0,
            ctx_used_tokens: 0,
            processing_start: None,
            tokens_processed: 0,
            tps: 0.0,
            api_prompt_tokens: None,
            api_completion_tokens: None,
            api_total_tokens: None,
            api_tps: 0.0,
            generation_active_time: 0.0,
            last_token_time: None,
            last_turn_end: None,
            pending_confirmation: None,
            needs_redraw: true,
            mode_menu_active: false,
            selected_mode_idx: 0,
            approval_modes: [
                "Babysitter - Always Ask",
                "Spring Break - Yolo for remainder of session",
                "Vegas - Yolo in sandbox",
                "Thunderdome - eternal Yolo, anytime, anywhere",
            ],
            live_exec_mode: std::sync::Arc::new(std::sync::Mutex::new(
                raven_tui::session::ExecApprovalMode::Babysitter,
            )),
            agent_mode_menu_active: false,
            selected_agent_mode_idx: 0,
            agent_modes: ["talk", "think", "research", "work", "dream", "plan"],
            slash_commands: crate::input_dispatch::default_slash_commands(),
            slash_selected: 0,
            display_model: config.model.clone(),
            display_budget: config.context_budget.clone(),
            balance_label: if raven_tui::llm::is_metered_endpoint(&config.base_url) {
                "$…".to_string()
            } else {
                "$∞".to_string()
            },
            settings: SettingsModal::inactive(),
            last_left_line_count: 0,
            last_right_line_count: 0,
            last_left_area: ratatui::layout::Rect::default(),
            last_right_area: ratatui::layout::Rect::default(),
            input_history: vec![],
            history_index: None,
            search: SearchState::default(),
            search_mode: false,
            cached_mode_label: String::new(),
            cached_goal_text: "none".into(),
            cached_agent_mode: "talk".into(),
            desktop: DesktopState::new(),
            raven_art: crate::desktop::load_raven_art(),
            splash_focus: SplashFocus::Magenta,
            view_focus: ViewFocus::Picker,
            browser_nav_items: vec![],
            browser_selected_nav: 0,
            browser_wiki_content: String::new(),
            browser_wiki_scroll: 0,
            picker: PickerState {
                current_wiki_file: "index.md".to_string(),
                current_wiki_content: String::new(),
                wiki_links: vec![],
                active_link_idx: 0,
                wiki_content_start: 0,
                last_summary_height: 20,
                show_wiki_in_summary: false,
                summary_action: SummaryAction::ViewWiki,
                ..Default::default()
            },
            wiki_viewer: WikiViewerState::default(),
            plan: PlanState::default(),
        }
    }

    pub fn plan_entry_dialog_open(&self) -> bool {
        self.pending_confirmation
            .as_ref()
            .is_some_and(|d| d.is_plan_entry())
    }

    pub fn try_slide_to_splash(&mut self) -> bool {
        if !self.desktop.can_slide_to_splash() {
            return false;
        }
        if !matches!(self.focused_pane, Pane::Left | Pane::Right) {
            return false;
        }
        let pane = match self.focused_pane {
            Pane::Left => crate::desktop::WorkspacePane::Left,
            Pane::Right => crate::desktop::WorkspacePane::Right,
            Pane::Input => return false,
        };
        self.desktop.start_slide_to_splash(pane);
        self.needs_redraw = true;
        true
    }

    pub fn try_slide_to_workspace(&mut self) -> bool {
        if !self.desktop.can_slide_to_workspace() {
            return false;
        }
        self.focused_pane = match self.desktop.workspace_pane {
            crate::desktop::WorkspacePane::Left => Pane::Left,
            crate::desktop::WorkspacePane::Right => Pane::Right,
        };
        self.desktop.start_slide_to_workspace();
        self.needs_redraw = true;
        true
    }

    #[allow(dead_code)]
    pub fn route_left_to_desktop(&self) -> bool {
        matches!(self.focused_pane, Pane::Left | Pane::Right)
            && self.desktop.can_slide_to_splash()
            && !self.desktop.is_animating()
    }

    #[allow(dead_code)]
    pub fn route_right_to_desktop(&self) -> bool {
        self.desktop.can_slide_to_workspace()
            && !self.desktop.is_animating()
            && !matches!(self.focused_pane, Pane::Input)
    }

    /// Insert a character at the current cursor position.
    pub fn insert_char(&mut self, c: char) {
        self.clamp_cursor();
        self.input.insert(self.cursor_pos, c);
        self.cursor_pos += c.len_utf8();
    }

    pub fn insert_str_at_cursor(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        self.clamp_cursor();
        self.input.insert_str(self.cursor_pos, s);
        self.cursor_pos += s.len();
    }

    pub fn apply_edit_action(&mut self, action: EditAction) {
        match action {
            EditAction::Insert(c) => self.insert_char(c),
            EditAction::InsertStr(s) => self.insert_str_at_cursor(&s),
            EditAction::Backspace => self.delete_char_before(),
            EditAction::Delete => self.delete_char_at(),
            EditAction::Left => self.move_cursor_left(),
            EditAction::Right => self.move_cursor_right(),
            EditAction::Home => self.move_cursor_home(),
            EditAction::End => self.move_cursor_end(),
        }
    }

    pub fn paste_into_settings(&mut self, text: &str) {
        if !self.settings.active {
            return;
        }
        if matches!(
            self.settings.mode,
            crate::settings_modal::SettingsMode::Adding
                | crate::settings_modal::SettingsMode::Editing
        ) {
            self.settings
                .apply_edit_action(EditAction::InsertStr(text.to_string()));
            self.needs_redraw = true;
        }
    }

    pub fn paste_into_input(&mut self, text: &str) {
        let sanitized: String = text
            .chars()
            .filter(|c| *c == '\n' || *c == '\t' || !c.is_control())
            .collect();
        if sanitized.is_empty() {
            return;
        }
        self.insert_str_at_cursor(&sanitized);
        self.history_index = None;
        crate::input_handler::clamp_slash_selection(&self.slash_commands, &self.input, &mut self.slash_selected);
        self.needs_redraw = true;
    }

    pub fn handle_clipboard_paste_key(&mut self) {
        if let Some(text) = try_read_clipboard() {
            if self.settings.active {
                self.paste_into_settings(&text);
            } else {
                self.paste_into_input(&text);
            }
        }
    }

    /// Delete the character before the cursor (Backspace).
    pub fn delete_char_before(&mut self) {
        self.clamp_cursor();
        if self.cursor_pos == 0 {
            return;
        }
        // Find the previous char boundary
        let prev = self.input[..self.cursor_pos]
            .char_indices()
            .next_back()
            .map(|(i, _)| i)
            .unwrap_or(0);
        self.input.drain(prev..self.cursor_pos);
        self.cursor_pos = prev;
    }

    /// Delete the character at the cursor (Delete key).
    pub fn delete_char_at(&mut self) {
        self.clamp_cursor();
        if self.cursor_pos >= self.input.len() {
            return;
        }
        let next = self.cursor_pos
            + self.input[self.cursor_pos..]
                .chars()
                .next()
                .map(|c| c.len_utf8())
                .unwrap_or(0);
        self.input.drain(self.cursor_pos..next);
    }

    /// Move cursor one character to the left.
    pub fn move_cursor_left(&mut self) {
        self.clamp_cursor();
        if self.cursor_pos == 0 {
            return;
        }
        self.cursor_pos = self.input[..self.cursor_pos]
            .char_indices()
            .next_back()
            .map(|(i, _)| i)
            .unwrap_or(0);
    }

    /// Move cursor one character to the right.
    pub fn move_cursor_right(&mut self) {
        self.clamp_cursor();
        if self.cursor_pos >= self.input.len() {
            return;
        }
        self.cursor_pos += self.input[self.cursor_pos..]
            .chars()
            .next()
            .map(|c| c.len_utf8())
            .unwrap_or(0);
    }

    /// Move cursor to the start of the input.
    pub fn move_cursor_home(&mut self) {
        self.cursor_pos = 0;
    }

    /// Move cursor to the end of the input.
    pub fn move_cursor_end(&mut self) {
        self.cursor_pos = self.input.len();
    }

    /// Replace the full input and put cursor at the end.
    pub fn set_input(&mut self, s: String) {
        self.input = s;
        self.cursor_pos = self.input.len();
    }

    /// Clear the input and reset cursor.
    pub fn clear_input(&mut self) {
        self.input.clear();
        self.cursor_pos = 0;
    }

    /// Keep cursor_pos within input bounds (guards against stale clears).
    pub fn clamp_cursor(&mut self) {
        self.cursor_pos = self.cursor_pos.min(self.input.len());
    }

    /// Submit the current input (when not processing).
    pub async fn submit_input(&mut self, _config: &raven_tui::config::Config, _agent: &Arc<tokio::sync::Mutex<Agent>>) -> anyhow::Result<()> {
        let text = self.input.trim().to_string();
        if text.is_empty() {
            return Ok(());
        }
        // For now, commit to left and clear (full agent dispatch happens in run_app / caller)
        self.left_committed.push(format!("> {}", text));
        self.clear_input();
        self.needs_redraw = true;
        Ok(())
    }

    /// Cycle focus forward: Left → Right → Input → Left
    pub fn cycle_focus_forward(&mut self) {
        self.focused_pane = match self.focused_pane {
            Pane::Left => Pane::Right,
            Pane::Right => Pane::Input,
            Pane::Input => Pane::Left,
        };
    }

    /// Cycle focus backward: Left → Input → Right → Left
    pub fn cycle_focus_backward(&mut self) {
        self.focused_pane = match self.focused_pane {
            Pane::Left => Pane::Input,
            Pane::Input => Pane::Right,
            Pane::Right => Pane::Left,
        };
    }

    pub(crate) fn pane_max_scroll(&self, pane: Pane) -> u16 {
        let (line_count, content_h) = match pane {
            Pane::Left => (
                self.last_left_line_count,
                self.last_left_area.height.saturating_sub(2),
            ),
            Pane::Right => (
                self.last_right_line_count,
                self.last_right_area.height.saturating_sub(2),
            ),
            Pane::Input => (0, 0),
        };
        line_count.saturating_sub(content_h)
    }

    /// Scroll the focused conversation/trace pane by `delta` lines (negative = up).
    pub fn scroll_focused_line(&mut self, delta: i16) {
        if self.desktop.showing_splash() || self.mode_menu_active || self.agent_mode_menu_active {
            return;
        }
        let pane = self.focused_pane;
        if !matches!(pane, Pane::Left | Pane::Right) {
            return;
        }
        self.scroll_pane_line(pane, delta);
    }

    /// Scroll the focused pane by `delta` pages (PgUp/PgDn).
    pub fn scroll_focused_page(&mut self, delta: i16, page_lines: u16) {
        if self.desktop.showing_splash() || self.mode_menu_active || self.agent_mode_menu_active {
            return;
        }
        let pane = self.focused_pane;
        if !matches!(pane, Pane::Left | Pane::Right) {
            return;
        }
        self.scroll_pane_page(pane, delta, page_lines);
    }

    pub fn scroll_pane_line(&mut self, pane: Pane, delta: i16) {
        let max_scroll = self.pane_max_scroll(pane);
        match pane {
            Pane::Left => {
                if delta < 0 {
                    self.left_follow_output = false;
                }
                let old_scroll = self.left_scroll;
                if delta < 0 {
                    self.left_scroll = self.left_scroll.saturating_sub(1);
                } else {
                    self.left_follow_output = false;
                    self.left_scroll = self.left_scroll.saturating_add(1);
                }
                if old_scroll == self.left_scroll
                    && ((delta < 0 && old_scroll == 0) || (delta > 0 && old_scroll >= max_scroll))
                {
                    self.scroll_flash_timer = 10;
                }
            }
            Pane::Right => {
                if delta < 0 {
                    self.right_follow_output = false;
                }
                let old_scroll = self.right_scroll;
                if delta < 0 {
                    self.right_scroll = self.right_scroll.saturating_sub(1);
                } else {
                    self.right_follow_output = false;
                    self.right_scroll = self.right_scroll.saturating_add(1);
                }
                if old_scroll == self.right_scroll
                    && ((delta < 0 && old_scroll == 0) || (delta > 0 && old_scroll >= max_scroll))
                {
                    self.scroll_flash_timer = 10;
                }
            }
            Pane::Input => return,
        }
        self.needs_redraw = true;
    }

    fn scroll_after_interject_ui(&mut self) {
        self.left_follow_output = true;
        self.left_scroll = 10_000;
        self.right_follow_output = true;
        self.right_scroll = 10_000;
        self.needs_redraw = true;
    }

    /// Queue an interject to apply before the next tool round (Enter while processing).
    pub fn submit_queued_interject(&mut self, text: String, queued: &Arc<std::sync::Mutex<Option<String>>>) {
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        if let Ok(mut slot) = queued.lock() {
            *slot = Some(text.clone());
        }
        self.left_committed
            .push(format!("You (interject, queued): {}", text));
        self.trace_lines
            .push("📌 Interject queued — will apply before next tool round".to_string());
        self.clear_input();
        self.history_index = None;
        self.scroll_after_interject_ui();
    }

    /// Stop inference and inject immediately (Ctrl+Enter while processing).
    pub fn submit_instant_interject(
        &mut self,
        text: String,
        queued: &Arc<std::sync::Mutex<Option<String>>>,
        instant: &Arc<std::sync::Mutex<Option<String>>>,
        stop: &Arc<AtomicBool>,
    ) {
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        if let Ok(mut slot) = queued.lock() {
            *slot = None;
        }
        if let Ok(mut slot) = instant.lock() {
            *slot = Some(text.clone());
        }
        stop.store(true, Ordering::SeqCst);
        match self.pending_confirmation.take() {
            Some(crate::confirmation_dialog::ConfirmationDialog::ToolApproval {
                responder, ..
            }) => {
                let _ = responder.send(false);
            }
            Some(crate::confirmation_dialog::ConfirmationDialog::PlanEntry { .. }) => {}
            None => {}
        }
        self.left_committed
            .push(format!("You (interject, now): {}", text));
        self.trace_lines
            .push("⚡ Interject sent — stopping current inference".to_string());
        self.clear_input();
        self.history_index = None;
        self.scroll_after_interject_ui();
    }

    pub fn scroll_pane_page(&mut self, pane: Pane, delta: i16, page_lines: u16) {
        let page = page_lines;
        let max_scroll = self.pane_max_scroll(pane);
        match pane {
            Pane::Left => {
                self.left_follow_output = false;
                let old_scroll = self.left_scroll;
                if delta < 0 {
                    self.left_scroll = self.left_scroll.saturating_sub(page);
                } else {
                    self.left_scroll = self.left_scroll.saturating_add(page);
                }
                if old_scroll == self.left_scroll
                    && ((delta < 0 && old_scroll == 0) || (delta > 0 && old_scroll >= max_scroll))
                {
                    self.scroll_flash_timer = 10;
                }
            }
            Pane::Right => {
                self.right_follow_output = false;
                let old_scroll = self.right_scroll;
                if delta < 0 {
                    self.right_scroll = self.right_scroll.saturating_sub(page);
                } else {
                    self.right_scroll = self.right_scroll.saturating_add(page);
                }
                if old_scroll == self.right_scroll
                    && ((delta < 0 && old_scroll == 0) || (delta > 0 && old_scroll >= max_scroll))
                {
                    self.scroll_flash_timer = 10;
                }
            }
            Pane::Input => return,
        }
        self.needs_redraw = true;
    }

    /// Handle a key when a confirmation modal is open.
    pub fn handle_confirmation_key(
        &mut self,
        key: crossterm::event::KeyEvent,
    ) -> crate::confirmation_dialog::ConfirmationKeyOutcome {
        use crate::confirmation_dialog::{ConfirmationDialog, ConfirmationKeyOutcome};
        let Some(dialog) = self.pending_confirmation.take() else {
            return ConfirmationKeyOutcome::NotHandled;
        };
        let confirmed = match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => true,
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => false,
            _ => {
                self.pending_confirmation = Some(dialog);
                return ConfirmationKeyOutcome::Handled;
            }
        };
        match dialog {
            ConfirmationDialog::ToolApproval { responder, .. } => {
                let _ = responder.send(confirmed);
                self.left_committed.push(if confirmed {
                    "✅ Action approved".to_string()
                } else {
                    "⛔ Action denied".to_string()
                });
                self.left_follow_output = true;
                self.left_scroll = 10_000;
                ConfirmationKeyOutcome::Handled
            }
            ConfirmationDialog::PlanEntry { goal } => ConfirmationKeyOutcome::PlanEntry {
                goal,
                confirmed,
            },
        }
    }

    /// Handle a key when the /mode selection menu is open.
    /// Returns `true` if the key was consumed (caller should `continue`).
    pub async fn handle_mode_menu_key(
        &mut self,
        key: crossterm::event::KeyEvent,
        agent: &Arc<tokio::sync::Mutex<Agent>>,
    ) -> bool {
        if !self.mode_menu_active {
            return false;
        }
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                if self.selected_mode_idx > 0 {
                    self.selected_mode_idx -= 1;
                }
                self.needs_redraw = true;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.selected_mode_idx < 3 {
                    self.selected_mode_idx += 1;
                }
                self.needs_redraw = true;
            }
            KeyCode::Enter => {
                let chosen = self.approval_modes[self.selected_mode_idx];
                self.left_committed
                    .push(format!("Approval mode set to: {}", chosen));
                self.left_follow_output = true;
                self.left_scroll = 10_000;

                let mode = match self.selected_mode_idx {
                    0 => raven_tui::session::ExecApprovalMode::Babysitter,
                    1 => raven_tui::session::ExecApprovalMode::SpringBreak,
                    2 => raven_tui::session::ExecApprovalMode::Vegas,
                    3 => raven_tui::session::ExecApprovalMode::Thunderdome,
                    _ => return true,
                };
                if let Ok(mut ag) = agent.try_lock() {
                    ag.set_exec_approval_mode(mode);
                    if let Some(s) = &mut ag.session_mut() {
                        let _ = s.save_meta();
                    }
                }
                if let Ok(mut slot) = self.live_exec_mode.lock() {
                    *slot = mode;
                }
                self.mode_menu_active = false;
                self.clear_input();
                self.selected_mode_idx = 0;
                self.needs_redraw = true;
            }
            KeyCode::Esc => {
                self.mode_menu_active = false;
                self.clear_input();
                self.selected_mode_idx = 0;
                self.needs_redraw = true;
            }
            _ => {}
        }
        true
    }

    /// Handle a key when the /run-mode selection menu is open.
    /// Returns `true` if the key was consumed (caller should `continue`).
    pub async fn handle_agent_mode_menu_key(
        &mut self,
        key: crossterm::event::KeyEvent,
        agent: &Arc<tokio::sync::Mutex<Agent>>,
    ) -> bool {
        if !self.agent_mode_menu_active {
            return false;
        }
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                if self.selected_agent_mode_idx > 0 {
                    self.selected_agent_mode_idx -= 1;
                }
                self.needs_redraw = true;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.selected_agent_mode_idx < self.agent_modes.len() - 1 {
                    self.selected_agent_mode_idx += 1;
                }
                self.needs_redraw = true;
            }
            KeyCode::Enter => {
                let chosen = self.agent_modes[self.selected_agent_mode_idx];
                self.left_committed
                    .push(format!("Run mode set to: {}", chosen));
                self.left_follow_output = true;
                self.left_scroll = 10_000;

                if let Ok(mut ag) = agent.try_lock() {
                    ag.set_agent_mode(chosen);
                    if let Some(s) = &mut ag.session_mut() {
                        let _ = s.save_meta();
                    }
                }
                self.agent_mode_menu_active = false;
                self.clear_input();
                self.selected_agent_mode_idx = 0;
                self.needs_redraw = true;
            }
            KeyCode::Esc => {
                self.agent_mode_menu_active = false;
                self.clear_input();
                self.selected_agent_mode_idx = 0;
                self.needs_redraw = true;
            }
            _ => {}
        }
        true
    }

    /// Enter or refresh the session/workspace picker.
    pub fn enter_picker(&mut self) {
        self.desktop.set_picker();
        self.clear_input();
        self.focused_pane = Pane::Left;
        if !self.picker.loaded {
            self.refresh_picker();
        }
        self.needs_redraw = true;
    }

    pub fn exit_picker_to_main(&mut self) {
        self.desktop.exit_picker_to_splash();
        self.needs_redraw = true;
    }

    /// Populate wiki_viewer nav (and content) for the *currently selected* session in picker,
    /// without switching the desktop. Used by the splash->overview slide to show the wiki nav pane.
    pub fn prepare_overview_for_session(&mut self, agent: &std::sync::Arc<tokio::sync::Mutex<Agent>>) {
        if let Some(meta) = self.picker.sessions.get(self.picker.selected_session) {
            let id = meta.session_id.clone();

            // Build stable browser nav for Screen 2: Coding Harness top level, Wiki top level enclosing the wiki items (no index.md mention)
            self.rebuild_browser_nav_for_session(&id);

            // Populate conversation for display when Coding Harness selected in Screen 2
            if let Ok(sess) = raven_tui::session::Session::open(&id) {
                let recent = sess.load_recent_conversation(18);
                self.left_committed.clear();
                for (role, content) in recent {
                    let disp = if role == "user" {
                        format!("> {}", content)
                    } else {
                        raven_tui::llm::strip_xml_tool_call_blocks(&content)
                    };
                    if !disp.trim().is_empty() {
                        self.left_committed.push(disp);
                    }
                }
                if let Ok(mut ag) = agent.try_lock() {
                    if let Ok(loaded_sess) = raven_tui::session::Session::open(&id) {
                        *ag.session_mut() = Some(loaded_sess);
                    }
                }
            }
        }
    }

    pub(crate) fn rebuild_browser_nav_for_session(&mut self, session_id: &str) {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        let wiki_dir = std::path::PathBuf::from(&home)
            .join(".raven-hotel")
            .join("sessions")
            .join(session_id)
            .join("wiki");

        let index_content = std::fs::read_to_string(wiki_dir.join("index.md")).unwrap_or_default();

        self.browser_nav_items = crate::wiki_doc::build_browser_overview_nav(&index_content);
        self.browser_selected_nav = 0;
        self.browser_wiki_content = index_content;
        self.browser_wiki_scroll = 0;
    }

    fn update_browser_preview_from_nav(&mut self) {
        if let Some(item) = self.browser_nav_items.get(self.browser_selected_nav) {
            if item.kind == NavItemKind::Harness {
                self.browser_wiki_content.clear();
            } else {
                let sess_id = self.picker.sessions.get(self.picker.selected_session)
                    .map(|m| m.session_id.clone())
                    .unwrap_or_default();
                let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
                let path = std::path::PathBuf::from(&home)
                    .join(".raven-hotel")
                    .join("sessions")
                    .join(sess_id)
                    .join("wiki")
                    .join(&item.target_file);
                self.browser_wiki_content = std::fs::read_to_string(&path)
                    .unwrap_or_else(|_| format!("(could not read {})", item.target_file));
            }
        }
        self.browser_wiki_scroll = 0;
        self.needs_redraw = true;
    }

    /// Enter the full wiki viewer for the currently selected session in the picker.
    pub fn enter_wiki_viewer(&mut self) {
        if let Some(meta) = self.picker.sessions.get(self.picker.selected_session) {
            let id = meta.session_id.clone();
            self.wiki_viewer.session_id = id.clone();
            self.wiki_viewer.current_file = "index.md".to_string();
            self.wiki_viewer.focus = WikiFocus::Nav;
            self.wiki_viewer.scroll = 0;
            self.wiki_viewer.selected_nav = 0;

            // Load list of wiki files (best effort) -- recursive to support subdirs the agent may create (e.g. research/)
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
            let wiki_dir = std::path::PathBuf::from(home)
                .join(".raven-hotel").join("sessions").join(&id).join("wiki");
            fn collect_md(dir: &std::path::Path, base: &std::path::Path, out: &mut Vec<String>) {
                if let Ok(rd) = std::fs::read_dir(dir) {
                    for e in rd.flatten() {
                        let p = e.path();
                        if p.is_dir() {
                            collect_md(&p, base, out);
                        } else if p.extension().and_then(|e| e.to_str()) == Some("md") {
                            if let Ok(rel) = p.strip_prefix(base) {
                                out.push(rel.display().to_string());
                            }
                        }
                    }
                }
            }
            let mut files = vec![];
            collect_md(&wiki_dir, &wiki_dir, &mut files);
            files.sort();
            if files.is_empty() {
                files.push("index.md".to_string());
            } else {
                // Put the entrypoint index.md first in nav for easier "up" access
                if let Some(pos) = files.iter().position(|f| f == "index.md" || f.ends_with("/index.md") || f.ends_with("index.md")) {
                    let f = files.remove(pos);
                    files.insert(0, f);
                }
            }
            self.wiki_viewer.files = files;

            // Load initial content and build nav elements (links + headings)
            self.load_wiki_viewer_content();
            self.rebuild_wiki_viewer_nav();
            // Prefer nav selection on an entry for the starting file
            if let Some(pos) = self.wiki_viewer.nav_items.iter().position(|it| it.target_file == self.wiki_viewer.current_file) {
                self.wiki_viewer.selected_nav = pos;
            }

            self.desktop.set_wiki_viewer();
            self.needs_redraw = true;
        }
    }

    fn load_wiki_viewer_content(&mut self) {
        if self.wiki_viewer.session_id.is_empty() {
            return;
        }
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        let clean = crate::wiki_doc::normalize_wiki_path(&self.wiki_viewer.current_file);
        let path = std::path::PathBuf::from(home)
            .join(".raven-hotel")
            .join("sessions")
            .join(&self.wiki_viewer.session_id)
            .join("wiki")
            .join(&clean);
        self.wiki_viewer.content = std::fs::read_to_string(&path)
            .unwrap_or_else(|_| format!("(could not read {})", clean));
        self.wiki_viewer.current_file = clean;
        // nav rebuilt by caller after content change in most paths; safe to call here too
        self.rebuild_wiki_viewer_nav();
    }

    fn rebuild_wiki_viewer_nav(&mut self) {
        self.wiki_viewer.nav_items = crate::wiki_doc::build_viewer_nav(
            &self.wiki_viewer.content,
            &self.wiki_viewer.current_file,
        );
        if self.wiki_viewer.selected_nav >= self.wiki_viewer.nav_items.len() {
            self.wiki_viewer.selected_nav = 0;
        }
    }

    fn apply_wiki_nav_selection(&mut self, idx: usize) {
        if idx >= self.wiki_viewer.nav_items.len() {
            return;
        }
        let item = self.wiki_viewer.nav_items[idx].clone();
        self.wiki_viewer.selected_nav = idx;
        if item.kind == NavItemKind::Harness {
            // Special: no file load, the caller/draw will show harness UI (conv+status+input) to right of nav
            return;
        }
        let clean_target = crate::wiki_doc::normalize_wiki_path(&item.target_file);
        let clean_cur = crate::wiki_doc::normalize_wiki_path(&self.wiki_viewer.current_file);

        // Back or Link targeting a different file → load that file
        let is_cross_file = clean_target != clean_cur
            && matches!(item.kind, NavItemKind::Back | NavItemKind::Link);

        if is_cross_file {
            self.wiki_viewer.current_file = clean_target;
            self.load_wiki_viewer_content();
            // After rebuild, select first header if available
            if let Some(first) = self.wiki_viewer.nav_items.iter().position(|it| it.kind == NavItemKind::Header) {
                self.wiki_viewer.selected_nav = first;
            } else {
                self.wiki_viewer.selected_nav = 0;
            }
            self.wiki_viewer.scroll = 0;
        } else {
            // Same-file heading or link — just scroll to position
            self.wiki_viewer.scroll = item.scroll_to;
        }
    }

    /// Light scroll: if the currently selected nav item is in the current file
    /// (a heading or same-file link), scroll content to its position.
    /// Does NOT load files or rebuild nav — safe for Up/Down browsing.
    fn scroll_to_nav_if_current_file(&mut self) {
        if let Some(item) = self.wiki_viewer.nav_items.get(self.wiki_viewer.selected_nav) {
            let clean_target = crate::wiki_doc::normalize_wiki_path(&item.target_file);
            let clean_cur = crate::wiki_doc::normalize_wiki_path(&self.wiki_viewer.current_file);
            if clean_target == clean_cur {
                self.wiki_viewer.scroll = item.scroll_to;
            }
            // If it's a different file, don't scroll — user must Enter to load it
        }
    }

    fn align_picker_to_wiki_session(&mut self) {
        let want = self.wiki_viewer.session_id.clone();
        // find in current sessions list
        if let Some(pos) = self.picker.sessions.iter().position(|m| m.session_id == want) {
            if self.picker.selected_session != pos {
                self.picker.selected_session = pos;
            }
            return;
        }
        // may need refresh
        self.refresh_picker_sessions();
        if let Some(pos) = self.picker.sessions.iter().position(|m| m.session_id == want) {
            self.picker.selected_session = pos;
        }
    }

    /// Handle keys when in the full wiki viewer screen.
    /// Returns true if handled.
    pub fn handle_wiki_viewer_key(&mut self, key: KeyCode, agent: &std::sync::Arc<tokio::sync::Mutex<Agent>>) -> bool {
        if !self.desktop.showing_wiki_viewer() {
            return false;
        }

        match key {
            KeyCode::Left | KeyCode::Char('h') => {
                if self.wiki_viewer.focus == WikiFocus::Nav {
                    // left from nav in Screen 3 back to Screen 2 (nav focused)
                    self.desktop.set_overview();
                    self.view_focus = ViewFocus::Nav;
                    if self.browser_nav_items.is_empty() {
                        let sid = self.picker.sessions.get(self.picker.selected_session).map(|m| m.session_id.clone());
                        if let Some(sid) = sid {
                            self.rebuild_browser_nav_for_session(&sid);
                        }
                    }
                    self.needs_redraw = true;
                    true
                } else {
                    // move to nav
                    self.wiki_viewer.focus = WikiFocus::Nav;
                    self.needs_redraw = true;
                    true
                }
            }
            KeyCode::Right | KeyCode::Char('l') => {
                if self.wiki_viewer.focus == WikiFocus::Content || self.wiki_viewer.selected_is_harness() {
                    // Rightmost or on Coding Harness — move to full workspace screen with trace.
                    let sid = self.wiki_viewer.session_id.clone();
                    self.desktop.exit_wiki_viewer_to_workspace();
                    if !sid.is_empty() {
                        self.activate_session_by_id(&sid, agent);
                    }
                    self.needs_redraw = true;
                    true
                } else {
                    self.wiki_viewer.focus = WikiFocus::Content;
                    self.needs_redraw = true;
                    true
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if self.wiki_viewer.focus == WikiFocus::Nav {
                    if !self.wiki_viewer.nav_items.is_empty() {
                        let n = self.wiki_viewer.nav_items.len();
                        self.wiki_viewer.selected_nav =
                            if self.wiki_viewer.selected_nav == 0 { n - 1 }
                            else { self.wiki_viewer.selected_nav - 1 };
                        // Scroll content if the item is in the current file (heading/link)
                        self.scroll_to_nav_if_current_file();
                        self.needs_redraw = true;
                    }
                } else {
                    self.wiki_viewer.scroll = self.wiki_viewer.scroll.saturating_sub(1);
                    self.needs_redraw = true;
                }
                true
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.wiki_viewer.focus == WikiFocus::Nav {
                    if !self.wiki_viewer.nav_items.is_empty() {
                        let n = self.wiki_viewer.nav_items.len();
                        self.wiki_viewer.selected_nav = (self.wiki_viewer.selected_nav + 1) % n;
                        // Scroll content if the item is in the current file (heading/link)
                        self.scroll_to_nav_if_current_file();
                        self.needs_redraw = true;
                    }
                } else {
                    self.wiki_viewer.scroll = self.wiki_viewer.scroll.saturating_add(1);
                    self.needs_redraw = true;
                }
                true
            }
            KeyCode::Tab => {
                if self.wiki_viewer.focus == WikiFocus::Nav && !self.wiki_viewer.nav_items.is_empty() {
                    let n = self.wiki_viewer.nav_items.len();
                    let next = (self.wiki_viewer.selected_nav + 1) % n;
                    if next == 0 {
                        // Wrapped around — switch to Content pane
                        self.wiki_viewer.focus = WikiFocus::Content;
                    } else {
                        self.wiki_viewer.selected_nav = next;
                        self.scroll_to_nav_if_current_file();
                    }
                    self.needs_redraw = true;
                } else {
                    // From Content pane, Tab goes back to Nav
                    self.wiki_viewer.focus = WikiFocus::Nav;
                    self.needs_redraw = true;
                }
                true
            }
            KeyCode::Enter => {
                if self.wiki_viewer.focus == WikiFocus::Nav {
                    if self.wiki_viewer.selected_is_harness() {
                        let sid = self.wiki_viewer.session_id.clone();
                        self.desktop.exit_wiki_viewer_to_workspace();
                        if !sid.is_empty() {
                            self.activate_session_by_id(&sid, agent);
                        }
                        self.needs_redraw = true;
                    } else if !self.wiki_viewer.nav_items.is_empty() {
                        let idx = self.wiki_viewer.selected_nav;
                        self.apply_wiki_nav_selection(idx);
                        self.needs_redraw = true;
                    }
                }
                true
            }
            KeyCode::Backspace => {
                // Navigate back to index.md
                if self.wiki_viewer.current_file != "index.md" {
                    self.wiki_viewer.current_file = "index.md".to_string();
                    self.wiki_viewer.scroll = 0;
                    self.load_wiki_viewer_content();
                    self.rebuild_wiki_viewer_nav();
                    if let Some(pos) = self.wiki_viewer.nav_items.iter().position(|it| it.target_file == "index.md") {
                        self.wiki_viewer.selected_nav = pos;
                    } else {
                        self.wiki_viewer.selected_nav = 0;
                    }
                    self.needs_redraw = true;
                }
                true
            }
            KeyCode::Char('r') | KeyCode::Char('R') => {
                // reload current
                self.load_wiki_viewer_content();
                self.needs_redraw = true;
                true
            }
            _ => true, // consume all keys — don't leak to workspace navigation
        }
    }

    pub fn refresh_picker(&mut self) {
        if let Ok(wss) = raven_tui::session::list_workspaces() {
            self.picker.workspaces = wss;
            if self.picker.selected_workspace >= self.picker.workspaces.len() {
                self.picker.selected_workspace = 0;
            }
        }

        // Build combined tree items (workspaces + their sessions indented) so we can
        // eliminate the separate sessions column/pane.
        let mut items: Vec<PickerItem> = vec![];
        for (wi, ws) in self.picker.workspaces.iter().enumerate() {
            // Store full path; truncation happens at render time based on pane width
            let label = ws.path.display().to_string();
            items.push(PickerItem {
                depth: 0,
                label,
                workspace_idx: wi,
                session_idx: None,
                workspace_path: ws.path.clone(),
                session_id: None,
            });

            // Load sessions for this workspace to populate the tree (small N)
            if let Ok(ses) = raven_tui::session::list_sessions_for(&ws.path) {
                for (si, s) in ses.iter().enumerate().take(5) {
                    let short = &s.session_id[..s.session_id.len().min(18)];
                    let date = &s.updated_at[..s.updated_at.len().min(10)];
                    // Clean label; indent added in renderer based on depth.
                    let label = format!("{}  {}", short, date);
                    items.push(PickerItem {
                        depth: 1,
                        label,
                        workspace_idx: wi,
                        session_idx: Some(si),
                        workspace_path: ws.path.clone(),
                        session_id: Some(s.session_id.clone()),
                    });
                }
            }
        }
        self.picker.picker_items = items;
        if self.picker.selected_item >= self.picker.picker_items.len() {
            self.picker.selected_item = 0;
        }

        self.sync_picker_selection();
        self.refresh_picker_summary();
        self.picker.loaded = true;
    }

    pub fn refresh_picker_sessions(&mut self) {
        if let Some(ws) = self.picker.workspaces.get(self.picker.selected_workspace) {
            if let Ok(ses) = raven_tui::session::list_sessions_for(&ws.path) {
                self.picker.sessions = ses;
                if self.picker.selected_session >= self.picker.sessions.len() {
                    self.picker.selected_session = self.picker.sessions.len().saturating_sub(1);
                }
            } else {
                self.picker.sessions.clear();
                self.picker.selected_session = 0;
            }
        } else {
            self.picker.sessions.clear();
            self.picker.selected_session = 0;
        }
        self.refresh_picker_summary();
    }

    fn sync_picker_selection(&mut self) {
        if let Some(item) = self.picker.picker_items.get(self.picker.selected_item) {
            // Resolve workspace index by stable path when possible (handles list reorder)
            let wi = if let Some(pos) = self.picker.workspaces.iter().position(|w| w.path == item.workspace_path) {
                pos
            } else {
                item.workspace_idx.min(self.picker.workspaces.len().saturating_sub(1))
            };
            self.picker.selected_workspace = wi;

            // Always reload the sessions list for the resolved ws (newest first)
            let ws_path = self.picker.workspaces.get(wi).map(|w| w.path.clone()).unwrap_or_else(|| item.workspace_path.clone());
            if let Ok(ses) = raven_tui::session::list_sessions_for(&ws_path) {
                self.picker.sessions = ses;
            } else {
                self.picker.sessions.clear();
            }

            // Resolve selected session by stable id or by index, else default to first (newest)
            if let Some(ref sid) = item.session_id {
                if let Some(pos) = self.picker.sessions.iter().position(|m| &m.session_id == sid) {
                    self.picker.selected_session = pos;
                    return;
                }
            }
            if let Some(si) = item.session_idx {
                if si < self.picker.sessions.len() {
                    self.picker.selected_session = si;
                    return;
                }
            }
            if !self.picker.sessions.is_empty() {
                self.picker.selected_session = 0;
            }
        }
    }

    pub fn refresh_picker_summary(&mut self) {
        if let Some(meta) = self.picker.sessions.get(self.picker.selected_session) {
            let _tests = if meta.achievement_tests.is_empty() {
                "  (none)".to_string()
            } else {
                meta.achievement_tests.iter().map(|t| format!("  - {}", t)).collect::<Vec<_>>().join("\n")
            };
            let _pitfalls = if meta.pitfalls.is_empty() {
                "  (none)".to_string()
            } else {
                meta.pitfalls.iter().map(|p| format!("  - {}", p)).collect::<Vec<_>>().join("\n")
            };
            let _discoveries = if meta.discoveries.is_empty() {
                "  (none)".to_string()
            } else {
                meta.discoveries.iter().map(|d| format!("  - {}", d)).collect::<Vec<_>>().join("\n")
            };
            let wiki_file = if self.picker.current_wiki_file.is_empty() { "index.md".to_string() } else { self.picker.current_wiki_file.clone() };
            let session_id = meta.session_id.clone();
            let workspace = meta.workspace.clone();
            let agent_mode = meta.agent_mode.clone();
            let updated_at = meta.updated_at.clone();
            let current_goal = meta.current_goal.clone();
            let session_label = if meta.session_label.trim().is_empty() {
                "Undetermined".to_string()
            } else {
                meta.session_label.clone()
            };
            let recent_turns_summary = meta.recent_turns_summary.clone();
            let repo_short = meta.repo_cache.short_summary.clone();

            let summary_text = if self.picker.show_wiki_in_summary {
                // When Wiki button active, show ONLY the wiki markdown, no session meta
                let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
                let path = std::path::PathBuf::from(&home)
                    .join(".raven-hotel").join("sessions").join(&session_id)
                    .join("wiki").join(&wiki_file);
                match std::fs::read_to_string(&path) {
                    Ok(c) => {
                        self.picker.current_wiki_content = c.clone();
                        self.picker.wiki_links = crate::wiki_doc::extract_links(&c);
                        self.picker.wiki_content_start = 0;
                        self.recompute_active_link();

                        let head = if self.picker.focus == PickerFocus::Summary {
                            c.clone()
                        } else {
                            c.lines().take(100).collect::<Vec<_>>().join("\n")
                        };
                        head  // pure content for clean wiki display (no meta, no header line)
                    }
                    Err(_) => {
                        self.picker.current_wiki_content.clear();
                        self.picker.wiki_links.clear();
                        self.picker.active_link_idx = 0;
                        self.picker.wiki_content_start = 0;
                        self.recompute_active_link();
                        "(no wiki content)".to_string()
                    },
                }
            } else {
                // Normal session meta text
                let tests = if meta.achievement_tests.is_empty() {
                    "  (none)".to_string()
                } else {
                    meta.achievement_tests.iter().map(|t| format!("  - {}", t)).collect::<Vec<_>>().join("\n")
                };
                let pitfalls = if meta.pitfalls.is_empty() {
                    "  (none)".to_string()
                } else {
                    meta.pitfalls.iter().map(|p| format!("  - {}", p)).collect::<Vec<_>>().join("\n")
                };
                let discoveries = if meta.discoveries.is_empty() {
                    "  (none)".to_string()
                } else {
                    meta.discoveries.iter().map(|d| format!("  - {}", d)).collect::<Vec<_>>().join("\n")
                };
                format!(
                    "{}\n\nSession: {}\nWorkspace: {}\n\nMode: {}\nUpdated: {}\n\nGoal:\n  {}\n\nAchievement tests:\n{}\n\nPitfalls:\n{}\n\nDiscoveries:\n{}\n\nRecent summary:\n  {}\n\nRepo summary:\n  {}",
                    session_label,
                    session_id,
                    workspace.display(),
                    agent_mode,
                    updated_at,
                    current_goal,
                    tests,
                    pitfalls,
                    discoveries,
                    recent_turns_summary,
                    repo_short
                )
            };

            self.picker.summary = summary_text;
        } else if let Some(ws) = self.picker.workspaces.get(self.picker.selected_workspace) {
            self.picker.summary = format!(
                "Workspace: {}\n\nNo session selected.\n\nUse 'n' to create a new session,\n'a' to add new workspace,\n'd' to delete current."
            , ws.path.display());
            self.picker.wiki_content_start = 0;
            self.picker.wiki_links.clear();
            self.picker.active_link_idx = 0;
            self.recompute_active_link();
        } else {
            self.picker.summary = "No workspaces.\n\nPress 'a' to add a workspace.".to_string();
            self.picker.wiki_content_start = 0;
            self.picker.wiki_links.clear();
            self.picker.active_link_idx = 0;
            self.recompute_active_link();
        }
    }

    fn recompute_active_link(&mut self) {
        if self.picker.wiki_links.is_empty() {
            self.picker.active_link_idx = 0;
            return;
        }
        let total_scroll = self.picker.summary_scroll;
        let wiki_scroll = total_scroll.saturating_sub(self.picker.wiki_content_start);
        let visible_h = self.picker.last_summary_height.max(5) as usize;
        self.picker.active_link_idx = 0;
        for (i, link) in self.picker.wiki_links.iter().enumerate() {
            if link.line >= wiki_scroll && link.line < wiki_scroll + visible_h {
                self.picker.active_link_idx = i;
                break;
            }
        }
    }

    /// Simple link follower for the summary/wiki pane.
    /// Follows the currently active link (first visible one).
    fn follow_wiki_link_in_summary(&mut self) {
        if self.picker.focus != PickerFocus::Summary {
            return;
        }

        // Get the currently viewed wiki file for this session
        let wiki_file = if self.picker.current_wiki_file.is_empty() {
            "index.md".to_string()
        } else {
            self.picker.current_wiki_file.clone()
        };

        // Load the *actual full content* of the current wiki file for reliable link scanning
        // (the embedded preview in .summary is truncated)
        let _content = String::new();
        if let Some(meta) = self.picker.sessions.get(self.picker.selected_session) {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
            let path = std::path::PathBuf::from(&home)
                .join(".raven-hotel")
                .join("sessions")
                .join(&meta.session_id)
                .join("wiki")
                .join(&wiki_file);
            let _ = std::fs::read_to_string(&path);
        }

        // Use the active link (first visible one) instead of rescanning
        if let Some(link) = self.picker.wiki_links.get(self.picker.active_link_idx).cloned() {
            let mut target = link.target;
            if let Some(hash) = target.find('#') {
                target = target[..hash].to_string();
            }
            self.picker.current_wiki_file = target;
            self.picker.summary_scroll = 0;
            self.refresh_picker_summary();
        } else if self.picker.current_wiki_file != "index.md" {
            self.picker.current_wiki_file = "index.md".to_string();
            self.picker.show_wiki_in_summary = false;
            self.picker.summary_scroll = 0;
            self.refresh_picker_summary();
        }
    }

    pub(crate) fn browser_selected_is_harness(&self) -> bool {
        browser_nav_is_harness(&self.browser_nav_items, self.browser_selected_nav)
    }

    fn reset_left_pane_for_harness(&mut self) {
        self.left_follow_output = false;
        self.left_scroll = 0;
    }

    fn is_picker_key_active(&self) -> bool {
        self.desktop.showing_picker()
            || self.desktop.active == crate::desktop::ActiveDesktop::Splash
            || self.desktop.active == crate::desktop::ActiveDesktop::Overview
    }

    fn focus_overview_to_content(&mut self) {
        self.view_focus = ViewFocus::Content;
        if self.browser_selected_is_harness() {
            self.reset_left_pane_for_harness();
        }
    }

    fn activate_overview_harness_session(
        &mut self,
        agent: &std::sync::Arc<tokio::sync::Mutex<Agent>>,
    ) {
        let sid = self
            .picker
            .sessions
            .get(self.picker.selected_session)
            .map(|m| m.session_id.clone());
        if let Some(sid) = sid {
            self.activate_session_by_id(&sid, agent);
        } else {
            self.desktop.set_workspace();
        }
    }

    fn enter_wiki_from_overview_content(&mut self) {
        if let Some(item) = self.browser_nav_items.get(self.browser_selected_nav) {
            if item.kind != NavItemKind::Harness {
                self.wiki_viewer.session_id = self
                    .picker
                    .sessions
                    .get(self.picker.selected_session)
                    .map(|m| m.session_id.clone())
                    .unwrap_or_default();
                self.wiki_viewer.current_file = item.target_file.clone();
                self.load_wiki_viewer_content();
            }
        }
        self.enter_wiki_viewer();
    }

    /// On splash, when magenta pane is focused, only allow focus-switch keys.
    fn handle_splash_magenta_picker_key(&mut self, key: KeyCode) -> bool {
        use crate::app_state::{PickerFocus, SplashFocus};
        if self.desktop.active != crate::desktop::ActiveDesktop::Splash
            || self.splash_focus == SplashFocus::Picker
        {
            return false;
        }
        match key {
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Tab => {
                self.splash_focus = SplashFocus::Picker;
                self.picker.focus = PickerFocus::Tree;
                self.needs_redraw = true;
                true
            }
            KeyCode::Left | KeyCode::Char('h') | KeyCode::Esc => {
                self.needs_redraw = true;
                true
            }
            _ => true,
        }
    }

    fn handle_picker_trust_confirm_key(
        &mut self,
        key: KeyCode,
        agent: &std::sync::Arc<tokio::sync::Mutex<Agent>>,
    ) -> bool {
        let Some(p) = self.picker.confirm_trust_path.clone() else {
            return false;
        };
        match key {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                self.picker.confirm_trust_path = None;
                self.clear_input();
                match raven_tui::session::Session::init(&p) {
                    Ok(mut new_sess) => {
                        new_sess.meta.trusted = true;
                        let _ = new_sess.save_meta();
                        let _ = raven_tui::session::ensure_repo_cache(&mut new_sess);
                        if let Ok(mut ag) = agent.try_lock() {
                            *ag.session_mut() = Some(new_sess);
                        }
                        self.refresh_picker();
                        self.needs_redraw = true;
                    }
                    Err(e) => {
                        self.left_committed.push(format!("Error: {}", e));
                        self.needs_redraw = true;
                    }
                }
                true
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                self.picker.confirm_trust_path = None;
                self.clear_input();
                self.needs_redraw = true;
                true
            }
            _ => true,
        }
    }

    fn handle_overview_picker_key(
        &mut self,
        key: KeyCode,
        agent: &std::sync::Arc<tokio::sync::Mutex<Agent>>,
    ) -> bool {
        use crate::app_state::SplashFocus;
        match key {
            KeyCode::Up | KeyCode::Char('k') => {
                match self.view_focus {
                    ViewFocus::Picker => {
                        if self.picker.selected_item > 0 {
                            self.picker.selected_item -= 1;
                            self.sync_picker_selection();
                            self.refresh_picker_summary();
                            self.prepare_overview_for_session(agent);
                        }
                    }
                    ViewFocus::Nav => {
                        if self.browser_selected_nav > 0 {
                            self.browser_selected_nav -= 1;
                        }
                        self.update_browser_preview_from_nav();
                    }
                    ViewFocus::Content => {
                        if self.browser_selected_is_harness() {
                            self.left_scroll = self.left_scroll.saturating_sub(1);
                        } else {
                            self.browser_wiki_scroll = self.browser_wiki_scroll.saturating_sub(1);
                        }
                    }
                }
                self.needs_redraw = true;
                true
            }
            KeyCode::Down | KeyCode::Char('j') => {
                match self.view_focus {
                    ViewFocus::Picker => {
                        if self.picker.selected_item + 1 < self.picker.picker_items.len() {
                            self.picker.selected_item += 1;
                            self.sync_picker_selection();
                            self.refresh_picker_summary();
                            self.prepare_overview_for_session(agent);
                        }
                    }
                    ViewFocus::Nav => {
                        if self.browser_selected_nav + 1 < self.browser_nav_items.len() {
                            self.browser_selected_nav += 1;
                        }
                        self.update_browser_preview_from_nav();
                    }
                    ViewFocus::Content => {
                        if self.browser_selected_is_harness() {
                            self.left_scroll = self.left_scroll.saturating_add(1);
                        } else {
                            self.browser_wiki_scroll = self.browser_wiki_scroll.saturating_add(1);
                        }
                    }
                }
                self.needs_redraw = true;
                true
            }
            KeyCode::Left | KeyCode::Char('h') => {
                match self.view_focus {
                    ViewFocus::Picker => {
                        self.desktop.exit_overview_to_splash();
                        self.splash_focus = SplashFocus::Picker;
                    }
                    ViewFocus::Nav => {
                        self.view_focus = ViewFocus::Picker;
                    }
                    ViewFocus::Content => {
                        self.view_focus = ViewFocus::Nav;
                    }
                }
                self.needs_redraw = true;
                true
            }
            KeyCode::Right | KeyCode::Char('l') => {
                match self.view_focus {
                    ViewFocus::Picker => {
                        self.view_focus = ViewFocus::Nav;
                    }
                    ViewFocus::Nav => {
                        self.focus_overview_to_content();
                    }
                    ViewFocus::Content => {
                        if self.browser_selected_is_harness() {
                            self.activate_overview_harness_session(agent);
                        } else {
                            self.enter_wiki_from_overview_content();
                        }
                    }
                }
                self.needs_redraw = true;
                true
            }
            KeyCode::Enter => {
                if self.view_focus == ViewFocus::Content {
                    if self.browser_selected_is_harness() {
                        self.reset_left_pane_for_harness();
                        self.activate_overview_harness_session(agent);
                    } else {
                        self.enter_wiki_viewer();
                    }
                } else if self.view_focus == ViewFocus::Nav {
                    self.view_focus = ViewFocus::Content;
                } else {
                    let item = self.picker.picker_items.get(self.picker.selected_item).cloned();
                    self.sync_picker_selection();
                    if let Some(item) = item {
                        if item.session_id.is_some() || item.depth == 1 {
                            self.activate_selected_session(agent);
                        }
                    }
                }
                self.needs_redraw = true;
                true
            }
            KeyCode::Tab => {
                self.view_focus = match self.view_focus {
                    ViewFocus::Picker => ViewFocus::Nav,
                    ViewFocus::Nav => ViewFocus::Content,
                    ViewFocus::Content => ViewFocus::Picker,
                };
                if self.view_focus == ViewFocus::Content && self.browser_selected_is_harness() {
                    self.reset_left_pane_for_harness();
                }
                self.needs_redraw = true;
                true
            }
            _ => false,
        }
    }

    fn handle_picker_screen_key(
        &mut self,
        key: KeyCode,
        agent: &std::sync::Arc<tokio::sync::Mutex<Agent>>,
    ) -> bool {
        use crate::app_state::{PickerFocus, SplashFocus};
        match key {
            KeyCode::Up | KeyCode::Char('k') => {
                match self.picker.focus {
                    PickerFocus::Tree => {
                        if self.picker.selected_item > 0 {
                            self.picker.selected_item -= 1;
                            self.sync_picker_selection();
                            self.refresh_picker_summary();
                        }
                    }
                    PickerFocus::Summary => {
                        self.picker.summary_scroll = self.picker.summary_scroll.saturating_sub(1);
                        self.recompute_active_link();
                        self.needs_redraw = true;
                    }
                }
                self.needs_redraw = true;
                true
            }
            KeyCode::Down | KeyCode::Char('j') => {
                match self.picker.focus {
                    PickerFocus::Tree => {
                        if self.picker.selected_item + 1 < self.picker.picker_items.len() {
                            self.picker.selected_item += 1;
                            self.sync_picker_selection();
                            self.refresh_picker_summary();
                        }
                    }
                    PickerFocus::Summary => {
                        self.picker.summary_scroll = self.picker.summary_scroll.saturating_add(1);
                        self.recompute_active_link();
                        self.needs_redraw = true;
                    }
                }
                self.needs_redraw = true;
                true
            }
            KeyCode::Left | KeyCode::Char('h') => {
                if self.desktop.active == crate::desktop::ActiveDesktop::Splash
                    && self.splash_focus == SplashFocus::Picker
                {
                    self.splash_focus = SplashFocus::Magenta;
                    self.needs_redraw = true;
                    return true;
                }
                // on magenta, left may exit to prior (workspace) via caller, fallthrough ok
                match self.picker.focus {
                    PickerFocus::Summary => {
                        self.picker.focus = PickerFocus::Tree;
                    }
                    _ => {
                        self.exit_picker_to_main();
                        self.splash_focus = SplashFocus::Magenta;
                    }
                }
                self.needs_redraw = true;
                true
            }
            KeyCode::Right | KeyCode::Char('l') => {
                match self.picker.focus {
                    PickerFocus::Tree => {
                        if self.desktop.active == crate::desktop::ActiveDesktop::Splash {
                            // Right from picker in Screen 1 -> Screen 2 (picker + nav + content)
                            self.prepare_overview_for_session(agent);
                            self.desktop.set_overview();
                            self.view_focus = ViewFocus::Picker;
                            self.wiki_viewer.session_id = self
                                .picker
                                .sessions
                                .get(self.picker.selected_session)
                                .map(|m| m.session_id.clone())
                                .unwrap_or_default();
                            self.wiki_viewer.focus = WikiFocus::Nav;
                            self.needs_redraw = true;
                            return true;
                        }
                        self.picker.focus = PickerFocus::Summary;
                        self.picker.summary_scroll = 0;
                        self.refresh_picker_summary();
                        self.recompute_active_link();
                    }
                    PickerFocus::Summary => {
                        // Right from Summary -> full wiki viewer for selected session
                        self.enter_wiki_viewer();
                    }
                }
                self.needs_redraw = true;
                true
            }
            KeyCode::Enter => {
                match self.picker.focus {
                    PickerFocus::Tree => {
                        let item = self.picker.picker_items.get(self.picker.selected_item).cloned();
                        self.sync_picker_selection();
                        if let Some(item) = item {
                            if item.session_id.is_some() || item.depth == 1 {
                                self.activate_selected_session(agent);
                            } else {
                                // workspace header row -> focus its (first/newest) session summary
                                self.picker.focus = PickerFocus::Summary;
                                self.refresh_picker_summary();
                            }
                        }
                    }
                    PickerFocus::Summary => {
                        let n_links = self.picker.wiki_links.len();
                        let idx = self.picker.active_link_idx;
                        if self.picker.show_wiki_in_summary && idx < n_links && n_links > 0 {
                            // Active item is a wiki link — follow it
                            self.follow_wiki_link_in_summary();
                        } else if idx == n_links {
                            // Wiki button -> full dedicated wiki viewer screen
                            self.enter_wiki_viewer();
                        } else if idx == n_links + 1 {
                            // Launch button
                            self.activate_selected_session(agent);
                        } else {
                            // No links, default to wiki viewer
                            self.enter_wiki_viewer();
                        }
                    }
                }
                self.needs_redraw = true;
                true
            }
            KeyCode::Tab => {
                if self.desktop.active == crate::desktop::ActiveDesktop::Splash
                    && self.splash_focus == SplashFocus::Picker
                {
                    // Tab while picker highlighted on splash: slide to 3-col
                    self.prepare_overview_for_session(agent);
                    self.desktop.set_overview();
                    self.wiki_viewer.focus = WikiFocus::Nav;
                    self.needs_redraw = true;
                    return true;
                }
                // Cycle through: wiki links → Wiki button → Launch button → back to links
                if self.picker.focus == PickerFocus::Summary {
                    let n_links = self.picker.wiki_links.len();
                    let total = n_links + 2; // +2 for Wiki and Launch buttons
                    self.picker.active_link_idx = (self.picker.active_link_idx + 1) % total;
                    let idx = self.picker.active_link_idx;

                    if idx < n_links {
                        // On a link — update summary_action to ViewWiki and scroll to it
                        self.picker.summary_action = SummaryAction::ViewWiki;
                        if let Some(link) = self.picker.wiki_links.get(idx) {
                            let visible_h = self.picker.last_summary_height.max(5) as usize;
                            if link.line < self.picker.summary_scroll
                                || link.line >= self.picker.summary_scroll + visible_h
                            {
                                self.picker.summary_scroll = link.line.saturating_sub(2);
                            }
                        }
                    } else if idx == n_links {
                        // Wiki button
                        self.picker.summary_action = SummaryAction::ViewWiki;
                    } else {
                        // Launch button
                        self.picker.summary_action = SummaryAction::Launch;
                    }
                    self.needs_redraw = true;
                }
                true
            }
            KeyCode::Backspace => {
                // Navigate back to index.md in wiki mode
                if self.picker.focus == PickerFocus::Summary
                    && self.picker.show_wiki_in_summary
                    && self.picker.current_wiki_file != "index.md"
                {
                    self.picker.current_wiki_file = "index.md".to_string();
                    self.picker.summary_scroll = 0;
                    self.refresh_picker_summary();
                    self.recompute_active_link();
                    self.needs_redraw = true;
                }
                true
            }
            KeyCode::Char('a') | KeyCode::Char('A') => {
                self.picker.adding_workspace = true;
                self.input.clear();
                self.cursor_pos = 0;
                self.needs_redraw = true;
                true
            }
            KeyCode::Char('n') | KeyCode::Char('N') => {
                // New session for current ws — the tree now owns selection (sessions live under ws rows)
                if let Some(ws) = self.picker.workspaces.get(self.picker.selected_workspace) {
                    let ws_path = ws.path.clone();
                    let name = format!("new-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs());
                    if let Ok(new_sess) = raven_tui::session::Session::init_named(&ws_path, &name) {
                        let new_id = new_sess.id.clone();
                        self.refresh_picker();
                        // Prefer stable session id to pick the correct tree row (newest first after sort)
                        if let Some(pos) = self.picker.picker_items.iter().position(|it| {
                            it.session_id.as_ref() == Some(&new_id)
                        }) {
                            self.picker.selected_item = pos;
                            self.sync_picker_selection();
                            self.refresh_picker_summary();
                        } else if let Some(pos) = self.picker.picker_items.iter().position(|it| {
                            it.workspace_path == ws_path && it.depth == 1
                        }) {
                            // Fallback: first session row under this ws (newest)
                            self.picker.selected_item = pos;
                            self.sync_picker_selection();
                            self.refresh_picker_summary();
                        }
                    }
                }
                self.needs_redraw = true;
                true
            }
            KeyCode::Char('d') | KeyCode::Char('D') => {
                // Delete current focus item (tree now combines ws + sessions)
                if self.picker.focus == PickerFocus::Tree {
                    if let Some(item) = self.picker.picker_items.get(self.picker.selected_item) {
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
                        self.refresh_picker();
                    }
                }
                self.needs_redraw = true;
                true
            }
            KeyCode::Char('w') | KeyCode::Char('W') => {
                if self.picker.focus == PickerFocus::Summary {
                    self.picker.summary_action = SummaryAction::ViewWiki;
                    self.picker.show_wiki_in_summary = !self.picker.show_wiki_in_summary;
                    if self.picker.show_wiki_in_summary {
                        self.refresh_picker_summary();
                        self.recompute_active_link();
                    }
                    self.needs_redraw = true;
                    return true;
                } else if self.picker.focus == PickerFocus::Tree {
                    // From tree, if on a session row, dump its wiki to main left pane (like old behavior)
                    if let Some(item) = self.picker.picker_items.get(self.picker.selected_item) {
                        if let Some(ref sid) = item.session_id {
                            let wiki_file = if self.picker.current_wiki_file.is_empty() {
                                "index.md".to_string()
                            } else {
                                self.picker.current_wiki_file.clone()
                            };
                            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
                            let wiki_path = std::path::PathBuf::from(home)
                                .join(".raven-hotel").join("sessions").join(sid)
                                .join("wiki").join(&wiki_file);
                            let content = std::fs::read_to_string(&wiki_path)
                                .unwrap_or_else(|_| format!("(no {wiki_file} yet — agent can create one with write_wiki)"));
                            self.left_committed.push(format!("=== Wiki: {} ===\n{}", wiki_file, content));
                            self.left_follow_output = true;
                            self.left_scroll = 10_000;
                            self.needs_redraw = true;
                            return true;
                        }
                    }
                }
                true
            }
            KeyCode::PageUp => {
                if self.picker.focus == PickerFocus::Tree || self.picker.focus == PickerFocus::Summary {
                    self.picker.summary_scroll = self.picker.summary_scroll.saturating_sub(12);
                    self.recompute_active_link();
                    self.needs_redraw = true;
                    return true;
                }
                false
            }
            KeyCode::PageDown => {
                if self.picker.focus == PickerFocus::Tree || self.picker.focus == PickerFocus::Summary {
                    self.picker.summary_scroll = self.picker.summary_scroll.saturating_add(12);
                    self.recompute_active_link();
                    self.needs_redraw = true;
                    return true;
                }
                false
            }
            _ => false,
        }
    }

    /// Switch focus or move selection in picker. Returns true if handled.
    pub fn handle_picker_key(
        &mut self,
        key: KeyCode,
        agent: &std::sync::Arc<tokio::sync::Mutex<Agent>>,
    ) -> bool {
        if !self.is_picker_key_active() {
            return false;
        }
        if self.handle_splash_magenta_picker_key(key) {
            return true;
        }
        if self.handle_picker_trust_confirm_key(key, agent) {
            return true;
        }
        if self.desktop.active == crate::desktop::ActiveDesktop::Overview {
            return self.handle_overview_picker_key(key, agent);
        }
        self.handle_picker_screen_key(key, agent)
    }

    fn activate_selected_session(&mut self, agent: &std::sync::Arc<tokio::sync::Mutex<Agent>>) {
        let sess_id = if let Some(meta) = self.picker.sessions.get(self.picker.selected_session) {
            meta.session_id.clone()
        } else {
            return;
        };
        self.activate_session_by_id(&sess_id, agent);
    }

    /// Load a specific session (by its persisted id) into the agent and prepopulate
    /// the workspace UI panes (conversation + resets trace). Used both by picker
    /// "launch" and when arrowing from wiki viewer into the programming screen.
    pub(crate) fn activate_session_by_id(&mut self, session_id: &str, agent: &std::sync::Arc<tokio::sync::Mutex<Agent>>) {
        let loaded_sess = match raven_tui::session::Session::open(session_id) {
            Ok(s) => s,
            Err(e) => {
                self.left_committed.push(format!("Error opening session {}: {}", session_id, e));
                self.needs_redraw = true;
                return;
            }
        };

        // Load recent conv history from log for UI display (prevents blank on re-select/reopen)
        self.left_committed.clear();
        let recent = loaded_sess.load_recent_conversation(25);
        for (role, content) in recent {
            let disp = if role == "user" {
                format!("> {}", content)
            } else {
                raven_tui::llm::strip_xml_tool_call_blocks(&content)
            };
            if !disp.trim().is_empty() {
                self.left_committed.push(disp);
            }
        }

        if let Ok(mut ag) = agent.try_lock() {
            *ag.session_mut() = Some(loaded_sess);
        }

        // Reset other visible UI state (trace is runtime-only; conv is restored above)
        self.trace_lines.clear();
        self.current_response.clear();
        self.current_thinking.clear();
        self.right_follow_output = true;
        self.right_scroll = 10_000;
        self.left_follow_output = true;
        self.left_scroll = 10_000;
        self.is_processing = false;
        self.focused_pane = Pane::Left;

        let banner = format!(
            "Raven Hotel - Loaded session\nSession: {}",
            session_id
        );
        self.left_committed.push(banner);

        // Switch to workspace view for the loaded session
        self.desktop.set_workspace();
        self.needs_redraw = true;
    }
}

fn remove_session_dir(session_id: &str) -> std::io::Result<()> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    let dir = std::path::PathBuf::from(home)
        .join(".raven-hotel")
        .join("sessions")
        .join(session_id);
    if dir.exists() {
        std::fs::remove_dir_all(dir)
    } else {
        Ok(())
    }
}


