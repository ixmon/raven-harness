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
pub use crate::wiki_browser::{WikiBrowser, WikiFocus};
pub use crate::wiki_doc::{NavItemKind, WikiLink};

/// Back-compat alias for render code.
pub type WikiViewerState = WikiBrowser;

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

    // Trace pane cursor (interactive navigation + fold/unfold)
    pub trace_cursor: usize,       // highlighted line index in trace_lines
    pub trace_cursor_active: bool, // true once user explicitly moves cursor
    pub trace_expanded: std::collections::HashSet<usize>, // header indices of expanded blocks
    /// Active scrollbar thumb drag (left or right workspace pane).
    pub scroll_drag_pane: Option<Pane>,

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
    /// Click targets for the current frame (updated during render).
    pub mouse_regions: crate::mouse_regions::MouseRegions,
    pub breadcrumb_segments: Vec<crate::tui_render::BreadcrumbClickSegment>,

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
    /// Splash layout diagram loaded from `/tmp/chunk2` (or `/tmp/chunk`, or default).
    pub splash_chunk: String,
    /// Cycling tips for the upper-right of the splash magenta pane.
    pub splash_tips: crate::splash_tips::SplashTipsState,

    /// Focus on the splash (first) screen: magenta pane (default) or workspace picker.
    pub splash_focus: SplashFocus,

    /// Focus for the picker+nav+content view (Screen 2 after sliding from splash).
    pub view_focus: ViewFocus,

    /// Screen 2 middle/right wiki preview (stable Harness + Wiki tree).
    pub overview_browser: WikiBrowser,

    // Session / workspace picker (new screen to the right of splash)
    pub picker: PickerState,

    // Full wiki viewer screen (Screen 3)
    pub wiki_viewer: WikiBrowser,

    // Plan Mode state (new pane + run mode "plan")
    pub plan: PlanState,

    // Session ID for trace persistence (set at startup, avoids needing agent lock for appends)
    pub session_id: Option<String>,
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
            trace_cursor: 0,
            trace_cursor_active: false,
            trace_expanded: std::collections::HashSet::new(),
            scroll_drag_pane: None,
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
            mouse_regions: crate::mouse_regions::MouseRegions::default(),
            breadcrumb_segments: Vec::new(),
            input_history: vec![],
            history_index: None,
            search: SearchState::default(),
            search_mode: false,
            cached_mode_label: String::new(),
            cached_goal_text: "none".into(),
            cached_agent_mode: "talk".into(),
            desktop: DesktopState::new(),
            raven_art: crate::desktop::load_raven_art(),
            splash_chunk: crate::desktop::load_splash_chunk(),
            splash_tips: crate::splash_tips::SplashTipsState::new(crate::splash_tips::load_splash_tips()),
            splash_focus: SplashFocus::Magenta,
            view_focus: ViewFocus::Picker,
            overview_browser: WikiBrowser::default(),
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
            wiki_viewer: WikiBrowser::default(),
            plan: PlanState::default(),
            session_id: config.prebuilt_session.as_ref().map(|s| s.id.clone()),
        }
    }

    /// Persist a trace event to trace_log.jsonl (best-effort, never fails loudly).
    pub fn persist_trace_event(&self, event: &raven_tui::session::TraceEvent) {
        if let Some(ref sid) = self.session_id {
            if let Ok(sess) = raven_tui::session::Session::open(sid) {
                let _ = sess.append_trace(event);
            }
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
        if pane == Pane::Right {
            self.move_trace_cursor(delta);
        } else {
            self.scroll_pane_line(pane, delta);
        }
    }

    fn trace_visible_lines(&self) -> Vec<crate::trace_fold::VisibleLine> {
        let blocks = crate::trace_fold::detect_tool_blocks(&self.trace_lines);
        crate::trace_fold::compute_visible_lines(&self.trace_lines, &blocks, &self.trace_expanded)
    }

    pub(crate) fn right_pane_content_height(&self) -> u16 {
        self.last_right_area
            .height
            .saturating_sub(3)
            .max(1)
    }

    pub(crate) fn left_pane_content_height(&self) -> u16 {
        self.last_left_area
            .height
            .saturating_sub(3)
            .max(1)
    }

    fn right_max_scroll(&self) -> u16 {
        self.last_right_line_count
            .saturating_sub(self.right_pane_content_height())
    }

    fn left_max_scroll(&self) -> u16 {
        self.last_left_line_count
            .saturating_sub(self.left_pane_content_height())
    }

    /// Place the trace cursor on the first visible row in the viewport.
    pub fn activate_trace_cursor_in_viewport(&mut self) {
        if self.trace_lines.is_empty() {
            return;
        }
        let visible = self.trace_visible_lines();
        if visible.is_empty() {
            return;
        }
        let vis_idx = (self.right_scroll as usize).min(visible.len().saturating_sub(1));
        self.trace_cursor_active = true;
        self.trace_cursor = crate::trace_fold::cursor_line_for_visible(&visible, vis_idx);
        self.right_follow_output = false;
        self.needs_redraw = true;
    }

    /// Move the trace cursor up/down in visible (fold-aware) line space.
    fn move_trace_cursor(&mut self, delta: i16) {
        let visible = self.trace_visible_lines();
        if visible.is_empty() {
            return;
        }
        let mut vis_idx = if self.trace_cursor_active {
            let blocks = crate::trace_fold::detect_tool_blocks(&self.trace_lines);
            crate::trace_fold::visible_index_for_cursor(&visible, self.trace_cursor, &blocks)
        } else {
            self.trace_cursor_active = true;
            (self.right_scroll as usize).min(visible.len().saturating_sub(1))
        };

        let old_vis = vis_idx;
        if delta < 0 {
            self.right_follow_output = false;
            vis_idx = vis_idx.saturating_sub(1);
        } else {
            self.right_follow_output = false;
            vis_idx = (vis_idx + 1).min(visible.len().saturating_sub(1));
        }
        if old_vis == vis_idx {
            self.scroll_flash_timer = 10;
        }

        self.trace_cursor = crate::trace_fold::cursor_line_for_visible(&visible, vis_idx);

        let margin: u16 = 2;
        let content_h = self.right_pane_content_height();
        let vis = vis_idx as u16;
        if vis < self.right_scroll + margin {
            self.right_scroll = vis.saturating_sub(margin);
        } else if vis >= self.right_scroll + content_h.saturating_sub(margin) {
            self.right_scroll = vis.saturating_sub(content_h.saturating_sub(margin + 1));
        }
        self.right_scroll = self.right_scroll.min(self.right_max_scroll());
        self.needs_redraw = true;
    }

    /// Deactivate the trace cursor (return to scroll-only mode).
    pub fn deactivate_trace_cursor(&mut self) {
        self.trace_cursor_active = false;
        self.needs_redraw = true;
    }

    /// Toggle fold on the tool block under the current trace cursor.
    pub fn toggle_trace_fold(&mut self) {
        let blocks = crate::trace_fold::detect_tool_blocks(&self.trace_lines);
        let visible = crate::trace_fold::compute_visible_lines(
            &self.trace_lines,
            &blocks,
            &self.trace_expanded,
        );
        let vis_idx = crate::trace_fold::visible_index_for_cursor(
            &visible,
            self.trace_cursor,
            &blocks,
        );
        if let Some(vline) = visible.get(vis_idx) {
            if let Some(header_idx) =
                crate::trace_fold::fold_toggle_header(&self.trace_lines, &blocks, vline)
            {
                self.toggle_trace_fold_header(header_idx);
            }
        }
    }

    pub fn toggle_trace_fold_header(&mut self, header_idx: usize) {
        if self.trace_expanded.contains(&header_idx) {
            self.trace_expanded.remove(&header_idx);
        } else {
            self.trace_expanded.insert(header_idx);
        }
        self.needs_redraw = true;
    }

    /// Jump scroll position from a click/drag on the pane scrollbar track.
    pub fn scroll_pane_from_row(&mut self, pane: Pane, row: u16) {
        let (area, line_count, content_h, scroll, follow) = match pane {
            Pane::Left => (
                self.last_left_area,
                self.last_left_line_count,
                self.left_pane_content_height(),
                &mut self.left_scroll,
                &mut self.left_follow_output,
            ),
            Pane::Right => (
                self.last_right_area,
                self.last_right_line_count,
                self.right_pane_content_height(),
                &mut self.right_scroll,
                &mut self.right_follow_output,
            ),
            Pane::Input => return,
        };
        if line_count <= content_h || area.height == 0 {
            return;
        }
        *follow = false;
        let content_y = area.y + 1;
        let track_h = content_h.max(1) as f32;
        let rel = (row.saturating_sub(content_y) as f32 / track_h).clamp(0.0, 1.0);
        let max_scroll = line_count.saturating_sub(content_h);
        *scroll = (rel * max_scroll as f32).round() as u16;
        *scroll = (*scroll).min(max_scroll);
        self.needs_redraw = true;
    }

    /// Toggle fold all: if any blocks are expanded, collapse all; otherwise expand all.
    pub fn toggle_trace_fold_all(&mut self) {
        let blocks = crate::trace_fold::detect_tool_blocks(&self.trace_lines);
        if self.trace_expanded.is_empty() {
            // Expand all non-error blocks (error blocks are always expanded)
            for b in &blocks {
                self.trace_expanded.insert(b.header_idx);
            }
        } else {
            self.trace_expanded.clear();
        }
        self.needs_redraw = true;
    }

    pub fn pane_page_lines(&self, pane: Pane) -> u16 {
        let h = match pane {
            Pane::Left => self.last_left_area.height,
            Pane::Right => self.last_right_area.height,
            Pane::Input => 0,
        };
        h.saturating_sub(2).max(5)
    }

    /// Scroll the focused conversation/trace pane to the top.
    pub fn scroll_focused_home(&mut self) {
        if self.desktop.showing_splash() || self.mode_menu_active || self.agent_mode_menu_active {
            return;
        }
        let pane = self.focused_pane;
        if !matches!(pane, Pane::Left | Pane::Right) {
            return;
        }
        match pane {
            Pane::Left => {
                self.left_follow_output = false;
                self.left_scroll = 0;
            }
            Pane::Right => {
                self.right_follow_output = false;
                self.right_scroll = 0;
                self.trace_cursor_active = false;
            }
            Pane::Input => return,
        }
        self.needs_redraw = true;
    }

    /// Scroll the focused conversation/trace pane to the bottom.
    pub fn scroll_focused_end(&mut self) {
        if self.desktop.showing_splash() || self.mode_menu_active || self.agent_mode_menu_active {
            return;
        }
        let pane = self.focused_pane;
        if !matches!(pane, Pane::Left | Pane::Right) {
            return;
        }
        let max = self.pane_max_scroll(pane);
        match pane {
            Pane::Left => {
                self.left_follow_output = false;
                self.left_scroll = max;
            }
            Pane::Right => {
                self.right_follow_output = false;
                self.right_scroll = max;
                self.trace_cursor_active = false;
            }
            Pane::Input => return,
        }
        self.needs_redraw = true;
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
            self.overview_browser.open_overview_session(&id);

            // Populate conversation for display when Coding Harness selected in Screen 2
            if let Ok(sess) = raven_tui::session::Session::open(&id) {
                let recent = sess.load_recent_conversation(18);
                self.left_committed =
                    crate::conversation_display::format_conversation_lines(&recent);
                // Restore trace pane
                let trace_events = sess.load_recent_trace(200);
                if !trace_events.is_empty() {
                    self.trace_lines = trace_events
                        .iter()
                        .map(|e| e.display.clone())
                        .collect();
                } else {
                    self.trace_lines.clear();
                }
                self.session_id = Some(id.clone());
                if let Ok(mut ag) = agent.try_lock() {
                    if let Ok(loaded_sess) = raven_tui::session::Session::open(&id) {
                        *ag.session_mut() = Some(loaded_sess);
                    }
                }
            }
        }
    }

    /// Enter the full wiki viewer for the currently selected session in the picker.
    pub fn enter_wiki_viewer(&mut self) {
        self.enter_wiki_viewer_at(None);
    }

    pub(crate) fn enter_wiki_viewer_at(&mut self, file: Option<&str>) {
        if let Some(meta) = self.picker.sessions.get(self.picker.selected_session) {
            if let Some(f) = file {
                self.wiki_viewer.seed_viewer_file(&meta.session_id, f);
                self.wiki_viewer.files =
                    raven_tui::session::collect_session_wiki_md_files(&meta.session_id);
                self.wiki_viewer.focus = WikiFocus::Nav;
                if let Some(pos) = self
                    .wiki_viewer
                    .nav_items
                    .iter()
                    .position(|it| it.target_file == self.wiki_viewer.current_file)
                {
                    self.wiki_viewer.selected_nav = pos;
                }
            } else {
                self.wiki_viewer.open_viewer_session(&meta.session_id);
            }
            self.desktop.set_wiki_viewer();
            self.needs_redraw = true;
        }
    }

    /// Jump to a breadcrumb desktop (trail step or nav-hint destination).
    pub fn navigate_to_breadcrumb(
        &mut self,
        target: crate::desktop::ActiveDesktop,
        agent: &std::sync::Arc<tokio::sync::Mutex<Agent>>,
    ) {
        if self.desktop.is_animating() {
            return;
        }

        use crate::desktop::ActiveDesktop;
        use crate::wiki_doc::NavItemKind;

        match target {
            ActiveDesktop::Splash => {
                self.desktop.jump_to_splash();
                self.splash_focus = SplashFocus::Magenta;
                if !self.picker.loaded {
                    self.refresh_picker();
                }
            }
            ActiveDesktop::Picker => match self.desktop.active {
                ActiveDesktop::Splash => {
                    self.splash_focus = SplashFocus::Picker;
                    self.picker.focus = PickerFocus::Tree;
                }
                ActiveDesktop::Picker => {
                    self.picker.focus = PickerFocus::Tree;
                }
                ActiveDesktop::Overview => {
                    self.view_focus = ViewFocus::Picker;
                }
                ActiveDesktop::Workspace | ActiveDesktop::WikiViewer => {
                    if !self.picker.loaded {
                        self.refresh_picker();
                    }
                    self.prepare_overview_for_session(agent);
                    self.desktop.set_overview();
                    self.view_focus = ViewFocus::Picker;
                }
            },
            ActiveDesktop::Overview => {
                if !self.picker.loaded {
                    self.refresh_picker();
                }
                if self.desktop.active == ActiveDesktop::WikiViewer {
                    self.desktop.set_overview();
                    self.view_focus = ViewFocus::Nav;
                    if self.overview_browser.nav_items.is_empty() {
                        let sid = self.wiki_viewer.session_id.clone();
                        if !sid.is_empty() {
                            self.overview_browser.open_overview_session(&sid);
                        }
                    }
                    self.align_picker_to_wiki_session();
                } else {
                    self.prepare_overview_for_session(agent);
                    self.desktop.set_overview();
                    self.view_focus = ViewFocus::Nav;
                }
            }
            ActiveDesktop::Workspace => {
                if let Some(pos) = self
                    .overview_browser
                    .nav_items
                    .iter()
                    .position(|it| it.kind == NavItemKind::Harness)
                {
                    self.overview_browser.selected_nav = pos;
                }
                self.activate_overview_harness_session(agent);
            }
            ActiveDesktop::WikiViewer => {
                if self.desktop.active == ActiveDesktop::Overview
                    && !self.overview_browser.selected_is_harness()
                {
                    self.enter_wiki_from_overview_content();
                } else {
                    self.enter_wiki_viewer();
                }
            }
        }
        self.needs_redraw = true;
    }

    pub(crate) fn align_picker_to_wiki_session(&mut self) {
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

    pub fn handle_wiki_viewer_key(
        &mut self,
        key: KeyCode,
        agent: &std::sync::Arc<tokio::sync::Mutex<Agent>>,
    ) -> bool {
        crate::wiki_handlers::handle_key(self, key, agent)
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

    pub(crate) fn sync_picker_selection(&mut self) {
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
                match raven_tui::session::read_session_wiki_file(&session_id, &wiki_file) {
                    Some(c) => {
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
                    None => {
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

    pub(crate) fn recompute_active_link(&mut self) {
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
    pub(crate) fn follow_wiki_link_in_summary(&mut self) {
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
            let _ = raven_tui::session::read_session_wiki_file(&meta.session_id, &wiki_file);
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
        self.overview_browser.selected_is_harness()
    }

    pub(crate) fn reset_left_pane_for_harness(&mut self) {
        self.left_follow_output = false;
        self.left_scroll = 0;
    }


    pub(crate) fn focus_overview_to_content(&mut self) {
        self.view_focus = ViewFocus::Content;
        if self.browser_selected_is_harness() {
            self.reset_left_pane_for_harness();
        }
    }

    pub(crate) fn activate_overview_harness_session(
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

    pub(crate) fn enter_wiki_from_overview_content(&mut self) {
        let file = self
            .overview_browser
            .nav_items
            .get(self.overview_browser.selected_nav)
            .filter(|it| it.kind != NavItemKind::Harness)
            .map(|it| it.target_file.clone());
        self.enter_wiki_viewer_at(file.as_deref());
    }



    pub fn handle_picker_key(
        &mut self,
        key: KeyCode,
        agent: &std::sync::Arc<tokio::sync::Mutex<Agent>>,
    ) -> bool {
        crate::picker_handlers::handle_key(self, key, agent)
    }

    pub(crate) fn activate_selected_session(&mut self, agent: &std::sync::Arc<tokio::sync::Mutex<Agent>>) {
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
        let recent = loaded_sess.load_recent_conversation(25);
        self.left_committed = crate::conversation_display::format_conversation_lines(&recent);

        // Restore trace pane from trace_log (before moving session to agent)
        let trace_events = loaded_sess.load_recent_trace(200);
        if !trace_events.is_empty() {
            self.trace_lines = trace_events
                .iter()
                .map(|e| e.display.clone())
                .collect();
        } else {
            self.trace_lines.clear();
        }
        self.trace_cursor = 0;
        self.trace_cursor_active = false;
        self.trace_expanded.clear();
        self.session_id = Some(session_id.to_string());

        if let Ok(mut ag) = agent.try_lock() {
            *ag.session_mut() = Some(loaded_sess);
        }

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


