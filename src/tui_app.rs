//! Basic ratatui chat TUI for the agent.
//!
//! - Scrollable message history (user, assistant, tool results, errors)
//! - Bottom input bar
//! - Live streaming of assistant tokens when possible
//! - Tool execution feedback shown inline

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    Terminal,
};
use std::io;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::mpsc;

use crate::agent::Agent;
use crate::agent_driver::TurnObserver;
use crate::config::Config;
use crate::desktop::{load_raven_art, DesktopState, WorkspacePane};
use crate::input_dispatch::{
    apply_settings_actions, default_slash_commands, dispatch_slash_command, SlashContext,
    SlashDispatch,
};
use crate::key_edit::{is_paste_key, map_key_to_edit, EditAction};
use crate::llm::{strip_xml_tool_call_blocks, ToolCall};
use crate::session::ExecApprovalMode;
use async_trait::async_trait;
use tokio::sync::oneshot;
use crate::search::SearchState;
use crate::settings_modal::{handle_settings_key, SettingsModal};

#[cfg(feature = "clipboard")]
use arboard::Clipboard;

fn get_filtered_commands<'a>(
    commands: &'a [crate::input_dispatch::SlashCommand],
    input: &str,
) -> Vec<&'a crate::input_dispatch::SlashCommand> {
    if !input.starts_with('/') {
        return vec![];
    }
    let prefix = &input[1..].to_lowercase();
    commands
        .iter()
        .filter(|cmd| prefix.is_empty() || cmd.name.starts_with(prefix))
        .collect()
}

fn clamp_slash_selection(
    commands: &[crate::input_dispatch::SlashCommand],
    input: &str,
    selected: &mut usize,
) {
    let filtered = get_filtered_commands(commands, input);
    if !filtered.is_empty() {
        *selected = (*selected).min(filtered.len().saturating_sub(1));
    } else {
        *selected = 0;
    }
}

#[cfg(feature = "clipboard")]
fn try_copy_to_clipboard(text: &str) {
    if let Ok(mut clipboard) = Clipboard::new() {
        let _ = clipboard.set_text(text.to_string());
    }
}

#[cfg(feature = "clipboard")]
fn try_read_clipboard() -> Option<String> {
    Clipboard::new()
        .ok()
        .and_then(|mut clipboard| clipboard.get_text().ok())
        .filter(|s| !s.is_empty())
}

#[cfg(not(feature = "clipboard"))]
fn try_read_clipboard() -> Option<String> {
    None
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Pane {
    Left,
    Right,
    Input,
}

/// Holds all the mutable UI state that used to be ~50 local `let mut` variables
/// inside `run_app`. This is the main refactor from the glm.md review.
struct App {
    // Conversation / output
    left_committed: Vec<String>,
    current_response: String,

    // Right pane
    trace_lines: Vec<String>,
    current_thinking: String,

    // Input
    input: String,
    cursor_pos: usize, // byte offset into `input`, always on a char boundary

    // Navigation / scroll
    left_scroll: u16,
    right_scroll: u16,
    left_follow_output: bool,
    right_follow_output: bool,
    focused_pane: Pane,
    scroll_flash_timer: u8,

    // Processing state
    is_processing: bool,
    spinner_tick: usize,
    tool_calls_this_turn: usize,
    turn_rounds: usize,
    ctx_used_tokens: u32,

    // Approval
    pending_approval: Option<String>,
    approval_responder: Option<tokio::sync::oneshot::Sender<bool>>,
    needs_redraw: bool,

    // Mode menu
    mode_menu_active: bool,
    selected_mode_idx: usize,
    approval_modes: [&'static str; 4],

    // Slash menu
    slash_commands: Vec<crate::input_dispatch::SlashCommand>,
    slash_selected: usize,

    // Display state (updated on endpoint switch)
    display_model: String,
    display_budget: crate::config::ContextBudget,
    balance_label: String,

    // Settings modal (extracted module)
    settings: SettingsModal,

    // Last draw layout (for scroll clamping in keys)
    last_left_line_count: u16,
    last_right_line_count: u16,
    last_left_area: ratatui::layout::Rect,
    last_right_area: ratatui::layout::Rect,

    // Input history for up/down recall (glm.md UX)
    input_history: Vec<String>,
    history_index: Option<usize>,

    // Conversation / trace search
    search: SearchState,
    search_mode: bool,

    // Cached status-bar labels to avoid flicker when agent lock is contended (glm.md)
    cached_mode_label: String,
    cached_goal_text: String,

    // Multi-desktop: workspace ↔ splash (left/right arrow slide)
    desktop: DesktopState,
    raven_art: String,
}

impl App {
    fn new(config: &Config) -> Self {
        let banner = format!(
            "Raven Hotel - Agent Harness\n\n\
             Endpoint: {}\n\
             Model:    {}\n\
             Workspace: {}\n\n\
             Session context (goal tracking disabled by default; enable with RAVEN_GOAL_TRACKING=1), and a safe repo cache (tree + importance + recent summary)\n\
             are now persisted under ~/.raven-hotel/ and injected on every turn.\n\
             The model can call update_goal(...) and record_discovery(...) when intent shifts.\n\
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
            is_processing: false,
            spinner_tick: 0,
            tool_calls_this_turn: 0,
            turn_rounds: 0,
            ctx_used_tokens: 0,
            pending_approval: None,
            approval_responder: None,
            needs_redraw: true,
            mode_menu_active: false,
            selected_mode_idx: 0,
            approval_modes: [
                "Babysitter - Always Ask",
                "Spring Break - Yolo for remainder of session",
                "Vegas - Yolo in sandbox",
                "Thunderdome - eternal Yolo, anytime, anywhere",
            ],
            slash_commands: default_slash_commands(),
            slash_selected: 0,
            display_model: config.model.clone(),
            display_budget: config.context_budget.clone(),
            balance_label: if crate::llm::is_metered_endpoint(&config.base_url) {
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
            desktop: DesktopState::new(),
            raven_art: load_raven_art(),
        }
    }

    fn try_slide_to_splash(&mut self) -> bool {
        if !self.desktop.can_slide_to_splash() {
            return false;
        }
        if !matches!(self.focused_pane, Pane::Left | Pane::Right) {
            return false;
        }
        let pane = match self.focused_pane {
            Pane::Left => WorkspacePane::Left,
            Pane::Right => WorkspacePane::Right,
            Pane::Input => return false,
        };
        self.desktop.start_slide_to_splash(pane);
        self.needs_redraw = true;
        true
    }

    fn try_slide_to_workspace(&mut self) -> bool {
        if !self.desktop.can_slide_to_workspace() {
            return false;
        }
        self.focused_pane = match self.desktop.workspace_pane {
            WorkspacePane::Left => Pane::Left,
            WorkspacePane::Right => Pane::Right,
        };
        self.desktop.start_slide_to_workspace();
        self.needs_redraw = true;
        true
    }

    fn route_left_to_desktop(&self) -> bool {
        matches!(self.focused_pane, Pane::Left | Pane::Right)
            && self.desktop.can_slide_to_splash()
            && !self.desktop.is_animating()
    }

    fn route_right_to_desktop(&self) -> bool {
        self.desktop.can_slide_to_workspace()
            && !self.desktop.is_animating()
            && !matches!(self.focused_pane, Pane::Input)
    }
}

fn render_pane(pane: Pane) -> crate::tui_render::Pane {
    match pane {
        Pane::Left => crate::tui_render::Pane::Left,
        Pane::Right | Pane::Input => crate::tui_render::Pane::Right,
    }
}


/// Idle fallback interval for OpenRouter balance polling (no OpenRouter guidance on cadence).
const BALANCE_IDLE_REFRESH_SECS: u64 = 600;

fn schedule_balance_refresh(agent: &Arc<tokio::sync::Mutex<Agent>>, tx: &mpsc::Sender<String>) {
    let agent2 = agent.clone();
    let btx = tx.clone();
    tokio::spawn(async move {
        refresh_balance_label(&agent2, &btx).await;
    });
}

async fn refresh_balance_label(
    agent: &Arc<tokio::sync::Mutex<Agent>>,
    tx: &mpsc::Sender<String>,
) {
    let (base_url, api_key) = {
        let ag = agent.lock().await;
        let cfg = ag.current_config();
        (cfg.base_url.clone(), cfg.api_key.clone())
    };
    let label = crate::llm::balance_label_for(&base_url, api_key.as_deref()).await;
    let _ = tx.send(label).await;
}

async fn apply_settings_key(
    app: &mut App,
    key: crossterm::event::KeyEvent,
    config: &Config,
    keystore: &mut crate::keystore::Keystore,
    agent: &Arc<tokio::sync::Mutex<Agent>>,
    balance_tx: &mpsc::Sender<String>,
) {
    let result = handle_settings_key(&mut app.settings, key, config, keystore, agent).await;
    let endpoint_switched = result.actions.iter().any(|a| {
        matches!(
            a,
            crate::settings_modal::SettingsAction::DisplayUpdate { .. }
        )
    });
    apply_settings_actions(
        result.actions,
        &mut app.left_committed,
        &mut app.trace_lines,
        &mut app.display_model,
        &mut app.display_budget,
        &mut app.settings,
    );
    if endpoint_switched {
        if let Ok(ag) = agent.try_lock() {
            app.balance_label = if crate::llm::is_metered_endpoint(&ag.current_config().base_url) {
                "$…".to_string()
            } else {
                "$∞".to_string()
            };
        }
        schedule_balance_refresh(agent, balance_tx);
    }
    app.left_follow_output = true;
    app.left_scroll = 10_000;
    app.right_follow_output = true;
    app.right_scroll = 10_000;
    app.needs_redraw = true;
}

impl App {
    /// Insert a character at the current cursor position.
    fn insert_char(&mut self, c: char) {
        self.clamp_cursor();
        self.input.insert(self.cursor_pos, c);
        self.cursor_pos += c.len_utf8();
    }

    fn insert_str_at_cursor(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        self.clamp_cursor();
        self.input.insert_str(self.cursor_pos, s);
        self.cursor_pos += s.len();
    }

    fn apply_edit_action(&mut self, action: EditAction) {
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

    fn paste_into_settings(&mut self, text: &str) {
        if !self.settings.active {
            return;
        }
        if matches!(
            self.settings.mode,
            crate::settings_modal::SettingsMode::Adding | crate::settings_modal::SettingsMode::Editing
        ) {
            self.settings.apply_edit_action(EditAction::InsertStr(text.to_string()));
            self.needs_redraw = true;
        }
    }

    fn paste_into_input(&mut self, text: &str) {
        let sanitized: String = text
            .chars()
            .filter(|c| *c == '\n' || *c == '\t' || !c.is_control())
            .collect();
        if sanitized.is_empty() {
            return;
        }
        self.insert_str_at_cursor(&sanitized);
        self.history_index = None;
        clamp_slash_selection(&self.slash_commands, &self.input, &mut self.slash_selected);
        self.needs_redraw = true;
    }

    fn handle_clipboard_paste_key(&mut self) {
        if let Some(text) = try_read_clipboard() {
            if self.settings.active {
                self.paste_into_settings(&text);
            } else {
                self.paste_into_input(&text);
            }
        }
    }


    /// Delete the character before the cursor (Backspace).
    fn delete_char_before(&mut self) {
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
    fn delete_char_at(&mut self) {
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
    fn move_cursor_left(&mut self) {
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
    fn move_cursor_right(&mut self) {
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
    fn move_cursor_home(&mut self) {
        self.cursor_pos = 0;
    }

    /// Move cursor to the end of the input.
    fn move_cursor_end(&mut self) {
        self.cursor_pos = self.input.len();
    }

    /// Replace the full input and put cursor at the end.
    fn set_input(&mut self, s: String) {
        self.input = s;
        self.cursor_pos = self.input.len();
    }

    /// Clear the input and reset cursor.
    fn clear_input(&mut self) {
        self.input.clear();
        self.cursor_pos = 0;
    }

    /// Keep cursor_pos within input bounds (guards against stale clears).
    fn clamp_cursor(&mut self) {
        self.cursor_pos = self.cursor_pos.min(self.input.len());
    }

    /// Cycle focus forward: Left → Right → Input → Left
    fn cycle_focus_forward(&mut self) {
        self.focused_pane = match self.focused_pane {
            Pane::Left => Pane::Right,
            Pane::Right => Pane::Input,
            Pane::Input => Pane::Left,
        };
    }

    /// Cycle focus backward: Left → Input → Right → Left
    fn cycle_focus_backward(&mut self) {
        self.focused_pane = match self.focused_pane {
            Pane::Left => Pane::Input,
            Pane::Input => Pane::Right,
            Pane::Right => Pane::Left,
        };
    }

    fn pane_max_scroll(&self, pane: Pane) -> u16 {
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
    fn scroll_focused_line(&mut self, delta: i16) {
        if self.desktop.showing_splash() || self.mode_menu_active {
            return;
        }
        let pane = self.focused_pane;
        if !matches!(pane, Pane::Left | Pane::Right) {
            return;
        }
        self.scroll_pane_line(pane, delta);
    }

    /// Scroll the focused pane by `delta` pages (PgUp/PgDn).
    fn scroll_focused_page(&mut self, delta: i16, page_lines: u16) {
        if self.desktop.showing_splash() || self.mode_menu_active {
            return;
        }
        let pane = self.focused_pane;
        if !matches!(pane, Pane::Left | Pane::Right) {
            return;
        }
        self.scroll_pane_page(pane, delta, page_lines);
    }

    fn scroll_pane_line(&mut self, pane: Pane, delta: i16) {
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
    fn submit_queued_interject(&mut self, text: String, queued: &Arc<Mutex<Option<String>>>) {
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
    fn submit_instant_interject(
        &mut self,
        text: String,
        queued: &Arc<Mutex<Option<String>>>,
        instant: &Arc<Mutex<Option<String>>>,
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
        if let Some(tx) = self.approval_responder.take() {
            let _ = tx.send(false);
        }
        self.pending_approval = None;
        self.left_committed
            .push(format!("You (interject, now): {}", text));
        self.trace_lines
            .push("⚡ Interject sent — stopping current inference".to_string());
        self.clear_input();
        self.history_index = None;
        self.scroll_after_interject_ui();
    }

    fn scroll_pane_page(&mut self, pane: Pane, delta: i16, page_lines: u16) {
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
}

impl App {
    /// Handle a key when the approval dialog is open.
    /// Returns `true` if the key was consumed (caller should `continue`).
    fn handle_approval_key(&mut self, key: crossterm::event::KeyEvent) -> bool {
        if self.pending_approval.is_none() {
            return false;
        }
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                if let Some(tx) = self.approval_responder.take() {
                    let _ = tx.send(true);
                }
                self.pending_approval = None;
                self.left_committed.push("✅ Action approved".to_string());
                self.left_follow_output = true;
                self.left_scroll = 10_000;
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                if let Some(tx) = self.approval_responder.take() {
                    let _ = tx.send(false);
                }
                self.pending_approval = None;
                self.left_committed.push("⛔ Action denied".to_string());
                self.left_follow_output = true;
                self.left_scroll = 10_000;
            }
            _ => {}
        }
        true
    }

    /// Handle a key when the /mode selection menu is open.
    /// Returns `true` if the key was consumed (caller should `continue`).
    async fn handle_mode_menu_key(
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
                    .push(format!("Execution mode set to: {}", chosen));
                self.left_follow_output = true;
                self.left_scroll = 10_000;

                let mode = match self.selected_mode_idx {
                    0 => crate::session::ExecApprovalMode::Babysitter,
                    1 => crate::session::ExecApprovalMode::SpringBreak,
                    2 => crate::session::ExecApprovalMode::Vegas,
                    3 => crate::session::ExecApprovalMode::Thunderdome,
                    _ => return true,
                };
                if let Ok(mut ag) = agent.try_lock() {
                    ag.set_exec_approval_mode(mode);
                    if let Some(s) = &mut ag.session {
                        let _ = s.save_meta();
                    }
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
}

pub async fn run(
    config: Config,
    chat_backend: crate::chat_backend::ChatBackend,
    keystore: crate::keystore::Keystore,
) -> Result<()> {
    // Setup terminal cleanly.
    // We enter the alternate screen + raw mode with an explicit clear so that
    // any previous output (cargo warnings, shell history, etc.) does not show
    // "under" the TUI.
    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(
        stdout,
        crossterm::terminal::EnterAlternateScreen,
        crossterm::event::EnableMouseCapture,
        crossterm::event::EnableBracketedPaste,
        crossterm::terminal::Clear(crossterm::terminal::ClearType::All),
    )?;
    let backend = CrosstermBackend::new(stdout);
    let backend = crate::palette::PaletteBackend::new(backend);
    let mut terminal = Terminal::new(backend)?;

    // One extra clear via the terminal API (belt + suspenders)
    terminal.clear()?;

    let res = run_app(&mut terminal, config, chat_backend, keystore).await;

    // Restore terminal
    crossterm::terminal::disable_raw_mode()?;
    crossterm::execute!(
        terminal.backend_mut().inner_mut(),
        crossterm::event::DisableBracketedPaste,
        crossterm::terminal::LeaveAlternateScreen,
        crossterm::event::DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    if let Err(err) = res {
        eprintln!("TUI error: {:?}", err);
    }
    Ok(())
}

async fn run_app<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    config: Config,
    chat_backend: crate::chat_backend::ChatBackend,
    mut keystore: crate::keystore::Keystore,
) -> Result<()> {
    // Wrap agent in Arc<Mutex> so it persists across spawned turn tasks.
    // Previously each turn moved the agent into the task and recreated a blank
    // one afterward, losing all conversation history.
    let agent = Arc::new(tokio::sync::Mutex::new(Agent::new(
        config.clone(),
        chat_backend,
    )));

    let mut app = App::new(&config);

    // Support for raven-eval --test ... --interactive : prefill the input with the test's prompt
    // so the user sees the TUI with the prompt ready and can press Enter to start.
    if let Some(pfile) = &config.harness.initial_prompt_file {
        if let Ok(text) = std::fs::read_to_string(pfile) {
            let text = text.trim().to_string();
            if !text.is_empty() {
                app.input = text;
                app.cursor_pos = app.input.len();
                app.needs_redraw = true;
            }
        }
    }

    // Channel for agent to push live updates into the TUI
    let (tx, mut rx) = mpsc::channel::<UiUpdate>(64);

    // Channel for input thread to send key events to main loop
    let (input_tx, mut input_rx) = mpsc::channel::<Event>(64);

    // Channel for execution approval requests (tool needs user OK before running)
    // Sender goes to agent task, receiver stays in UI loop.
    // Carries (description, responder) so UI can show dialog and respond with bool.
    let (approval_req_tx, mut approval_req_rx) = mpsc::channel::<(String, tokio::sync::oneshot::Sender<bool>)>(4);

    // Stop signal: UI sets this to true (Escape while processing), agent task checks it
    // at clean stopping points (before each LLM call, before each tool execution).
    let stop_signal = Arc::new(AtomicBool::new(false));
    let queued_interject: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let instant_interject: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

    let (balance_tx, mut balance_rx) = mpsc::channel::<String>(4);
    {
        let agent_bal = agent.clone();
        let btx = balance_tx.clone();
        tokio::spawn(async move {
            loop {
                refresh_balance_label(&agent_bal, &btx).await;
                tokio::time::sleep(Duration::from_secs(BALANCE_IDLE_REFRESH_SECS)).await;
            }
        });
    }

    // Spawn a dedicated thread for keyboard input (responsive even during LLM streaming).
    // Uses blocking_send() to bridge the sync thread → async tokio channel.
    let _input_handle = std::thread::spawn(move || {
        loop {
            if event::poll(Duration::from_millis(10)).unwrap_or(false) {
                if let Ok(ev) = event::read() {
                    if input_tx.blocking_send(ev).is_err() {
                        break; // Main loop dropped receiver, exit thread
                    }
                }
            }
        }
    });

    loop {
        while let Ok(label) = balance_rx.try_recv() {
            app.balance_label = label;
            app.needs_redraw = true;
        }

        // Advance spinner
        if app.is_processing {
           app.spinner_tick = app.spinner_tick.wrapping_add(1);
        }

        // Read current state from agent (try_lock to avoid blocking the draw).
        // app.ctx_used_tokens is NOT read here — it's pushed via UiUpdate::ContextUsage
        // from the agent task to avoid any lock contention during processing.
        // On lock contention, reuse cached labels instead of showing "…" to avoid flicker.
        let (mode_label, goal_text) = if let Ok(ag) = agent.try_lock() {
            let mode = ag.current_exec_mode().label().to_string();
            let goal = if config.flags.goal_tracking {
                ag.session.as_ref()
                    .and_then(|s| {
                        let g = s.meta.current_goal.as_str();
                        if g.is_empty() { None } else { Some(g.to_string()) }
                    })
                    .unwrap_or_else(|| "(no goal set)".into())
            } else {
                "none".into()
            };
            (mode, goal)
        } else {
            (app.cached_mode_label.clone(), app.cached_goal_text.clone())
        };
        app.cached_mode_label = mode_label.clone();
        app.cached_goal_text = goal_text.clone();

        // Draw only when needed (basic perf improvement per glm.md)
        if app.needs_redraw || app.is_processing || app.scroll_flash_timer > 0 || app.desktop.is_animating() {
           app.needs_redraw = false;
            let workspace_display = config.workspace.display().to_string();
            // Draw - split into status bar + content area + context gauge + input bar
            terminal.draw(|f| {
            let size = f.area();

            // Vertical layout: status bar (1) + content panes (fill) + context gauge (1) + input bar (dynamic)
            let show_gauge = app.is_processing;
            let gauge_h = if show_gauge { 1 } else { 0 };
            // Dynamic input height: 3 (single line) up to 8 (multiline)
            let input_line_count = app.input.lines().count().max(1) as u16;
            let input_h = (input_line_count + 2).clamp(3, 8); // +2 for borders
            let vertical = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(1),        // status bar
                    Constraint::Min(6),           // content panes
                    Constraint::Length(gauge_h),   // context gauge (only during processing)
                    Constraint::Length(input_h),   // input bar (dynamic height)
                ])
                .split(size);

            let status_area = vertical[0];
            let content_area = vertical[1];
            let gauge_area = vertical[2];
            let input_area = vertical[3];

            // ═══════════════════ STATUS BAR ═══════════════════
            let search_label = if app.desktop.showing_splash() {
                "→ workspace".to_string()
            } else {
                app.search.status_label()
            };
            crate::tui_render::draw_status_bar(f, status_area, &crate::tui_render::StatusBarData {
                display_model: &app.display_model,
                balance_label: &app.balance_label,
                ctx_used_tokens: app.ctx_used_tokens,
                budget: &app.display_budget,
                mode_label: &mode_label,
                goal_text: &goal_text,
                search_label: &search_label,
            });

            // ═══════════════════ CONTENT: workspace ↔ splash (slide) ═══════════════════
            let left_highlight = if app.search.active
                && app.search.pane == crate::tui_render::Pane::Left
                && !app.search.match_lines.is_empty()
            {
                Some(app.search.match_lines[app.search.match_idx])
            } else {
                None
            };
            let right_highlight = if app.search.active
                && app.search.pane == crate::tui_render::Pane::Right
                && !app.search.match_lines.is_empty()
            {
                Some(app.search.match_lines[app.search.match_idx])
            } else {
                None
            };

            let left_focused = app.focused_pane == Pane::Left;
            let right_focused = app.focused_pane == Pane::Right;

            crate::tui_render::draw_content_desktop(
                f,
                content_area,
                &app.desktop,
                &crate::tui_render::WorkspaceDrawData {
                    left_committed: &app.left_committed,
                    current_response: &app.current_response,
                    trace_lines: &app.trace_lines,
                    current_thinking: &app.current_thinking,
                    left_scroll: app.left_scroll,
                    right_scroll: app.right_scroll,
                    left_focused,
                    right_focused,
                    scroll_flash_timer: app.scroll_flash_timer,
                    left_highlight,
                    right_highlight,
                },
                &crate::tui_render::SplashData {
                    raven_art: &app.raven_art,
                    base_url: &config.base_url,
                    model: &app.display_model,
                    workspace: &workspace_display,
                },
                &mut app.last_left_area,
                &mut app.last_right_area,
                &mut app.last_left_line_count,
                &mut app.last_right_line_count,
                &mut app.left_scroll,
                &mut app.right_scroll,
                app.left_follow_output,
                app.right_follow_output,
            );

            // ═══════════════════ CONTEXT GAUGE ═══════════════════
            if show_gauge {
                crate::tui_render::draw_context_gauge(f, gauge_area, &crate::tui_render::ContextGaugeData {
                    turn_rounds: app.turn_rounds,
                    max_rounds: config.max_rounds,
                    tool_calls_this_turn: app.tool_calls_this_turn,
                });
            }

            // ═══════════════════ INPUT BAR ═══════════════════
            crate::tui_render::draw_input_bar(f, input_area, &crate::tui_render::InputBarData {
                input: &app.input,
                is_processing: app.is_processing,
                spinner_tick: app.spinner_tick,
                search_mode: app.search_mode,
                focused: app.focused_pane == Pane::Input,
            });

            // Overlays: approval popup, slash menu, mode menu, settings modal
            crate::tui_render::draw_overlays(
                f,
                size,
                input_area,
                &app.settings,
                app.pending_approval.as_deref(),
                &app.slash_commands,
                &app.input,
                app.slash_selected,
                app.mode_menu_active,
                &app.approval_modes,
                app.selected_mode_idx,
            );

            // Cursor in input — compute position from cursor_pos, handling multiline
            app.clamp_cursor();
            let text_before_cursor = &app.input[..app.cursor_pos];
            let cursor_line = text_before_cursor.matches('\n').count() as u16;
            let last_newline = text_before_cursor.rfind('\n').map(|i| i + 1).unwrap_or(0);
            let cursor_col = app.input[last_newline..app.cursor_pos].chars().count() as u16;
            f.set_cursor_position((input_area.x + 1 + cursor_col, input_area.y + 1 + cursor_line));
            
            // Decrement scroll flash timer
            if app.scroll_flash_timer > 0 {
              app.scroll_flash_timer = app.scroll_flash_timer.saturating_sub(1);
            }

            if app.desktop.tick() {
                app.needs_redraw = true;
            }
        })?;
        }

        // Poll for new approval requests from the agent background task
        while let Ok((desc, tx)) = approval_req_rx.try_recv() {
           app.pending_approval = Some(desc.clone());
           app.approval_responder = Some(tx);
           app.needs_redraw = true; // force a redraw so popup shows
           app.clear_input();
           app.trace_lines.push(format!("🔒 Approval requested for: {}", desc));
           app.left_committed.push(format!("🔒 Approval requested for: {}", desc));
           app.left_follow_output = true;
           app.left_scroll = 10_000;
           app.right_follow_output = true;
           app.right_scroll = 10_000;
        }
        // If we just picked up a NEW approval this iteration, force a redraw
        // so the popup is visible before we block on select! waiting for input.
        // The `app.needs_redraw` flag prevents a hot loop (only continue once).
        if app.needs_redraw {
           app.needs_redraw = false;
            continue; // → top of loop → draw → select
        }

        // Handle input + agent updates (non-blocking)
        if app.is_processing {
            // While processing we mostly listen for agent updates and a few keys
            tokio::select! {
                Some(update) = rx.recv() => {
                    // Always poll for approval requests on any agent update to ensure dialog shows promptly
                    while let Ok((desc, tx)) = approval_req_rx.try_recv() {
                       app.pending_approval = Some(desc.clone());
                       app.approval_responder = Some(tx);
                       app.clear_input();
                      app.trace_lines.push(format!("🔒 Approval requested for: {}", desc));
                       app.left_committed.push(format!("🔒 Approval requested for: {}", desc));
                      app.left_follow_output = true;
                      app.left_scroll = 10_000;
                      app.right_follow_output = true;
                      app.right_scroll = 10_000;
                    }
                    match update {
                        UiUpdate::Token(t) => {
                            // Regular content tokens → live on the LEFT pane (current turn output)
                           app.current_response.push_str(&t);
                           app.needs_redraw = true;
                          app.left_follow_output = true;
                          app.left_scroll = 10_000; // auto-scroll to bottom while streaming output
                        }
                        UiUpdate::Thinking(t) => {
                            // Accumulate small thinking chunks (models often send 1-3 tokens at a time)
                            // and only commit to app.trace_lines on reasonable boundaries.
                          app.current_thinking.push_str(&t);
                           app.needs_redraw = true;
                          app.right_follow_output = true;
                          app.right_scroll = 10_000; // auto-scroll trace pane on new thinking

                            // Flush heuristic: paragraph break, sentence terminator + space, or size limit.
                            // This turns "one word per line" into proper sentences/paragraphs in the trace.
                            let should_flush =
                              app.current_thinking.contains("\n\n") ||
                              app.current_thinking.ends_with(". ") ||
                              app.current_thinking.ends_with("! ") ||
                              app.current_thinking.ends_with("? ") ||
                              app.current_thinking.len() > 160;

                            if should_flush {
                                let block = app.current_thinking.trim().to_string();
                                if !block.is_empty() {
                                  app.trace_lines.push(format!("🧠 {}", block));
                                  app.right_follow_output = true;
                                  app.right_scroll = 10_000;
                                }
                              app.current_thinking.clear();
                            }
                        }
                        UiUpdate::ToolStart { name, args } => {
                            // Tool activity → RIGHT pane (debug)
                          app.trace_lines.push(format!("🔧 {}({})", name, truncate(&args, 90)));
                           app.tool_calls_this_turn += 1;
                          app.right_follow_output = true;
                          app.right_scroll = 10_000;
                        }
                        UiUpdate::ToolResult { name, summary } => {
                          if name == "system" && summary.contains("JUDGE") {
                              app.trace_lines.push(format!("   ⭐⭐ JUDGE: {}", truncate(&summary, 140)));
                          } else {
                              app.trace_lines.push(format!("   ↳ {} → {}", name, truncate(&summary, 120)));
                          }
                          app.right_follow_output = true;
                          app.right_scroll = 10_000;
                        }
                        UiUpdate::RoundLimitHit { continuation, max_continuations, exhausted } => {
                            let msg = if exhausted {
                                format!(
                                    "⏸ Round limit — exhausted auto-continue budget ({}/{}). Send another message to continue.",
                                    continuation, max_continuations
                                )
                            } else {
                                format!(
                                    "⟳ Round limit hit — auto-continuing ({}/{})...",
                                    continuation, max_continuations
                                )
                            };
                          app.trace_lines.push(msg);
                           app.turn_rounds += 1;
                          app.right_follow_output = true;
                          app.right_scroll = 10_000;
                        }
                        UiUpdate::Done { final_text } => {
                            // Turn complete. Flush the output.
                            // Use the Done's final_text as a robust fallback in case no individual
                            // Token updates were emitted by the stream (some llama.cpp configurations
                            // deliver the full response only in the final payload).
                            let mut output = if !final_text.trim().is_empty() {
                                // Prefer the cleaned version from the Done payload (already stripped of XML tool call syntax)
                                final_text
                            } else if !app.current_response.trim().is_empty() {
                                app.current_response.trim().to_string()
                            } else if !app.current_thinking.trim().is_empty() {
                                // Fallback: if the model delivered the response via the reasoning/thinking channel
                                // (no regular content tokens), use it so output appears in the conversation pane.
                                app.current_thinking.trim().to_string()
                            } else {
                                String::new()
                            };
                            if !output.trim().is_empty() {
                                output = strip_xml_tool_call_blocks(&output);
                            }
                            if !output.is_empty() {
                               app.left_committed.push(format!("Agent: {}", output));
                                #[cfg(feature = "clipboard")]
                                try_copy_to_clipboard(&output);
                              app.left_follow_output = true;
                              app.left_scroll = 10_000; // auto-scroll to bottom after agent response
                               app.current_response.clear();
                            }
                            // Flush any remaining live thinking (to right trace pane)
                            if !app.current_thinking.trim().is_empty() {
                              app.trace_lines.push(format!("🧠 {}", app.current_thinking.trim()));
                              app.current_thinking.clear();
                            }
                            if !app.trace_lines.is_empty() {
                              app.right_follow_output = true;
                              app.right_scroll = 10_000;
                            }
                          app.needs_redraw = true;
                          app.is_processing = false;
                          schedule_balance_refresh(&agent, &balance_tx);
                        }
                        UiUpdate::Error(e) => {
                            let msg = format!("⚠ ERROR: {}", e);
                          app.trace_lines.push(msg.clone());
                          app.right_follow_output = true;
                          app.right_scroll = 10_000;
                            // Flush any pending thinking on error
                            if !app.current_thinking.trim().is_empty() {
                              app.trace_lines.push(format!("🧠 {}", app.current_thinking.trim()));
                              app.current_thinking.clear();
                            }
                            // Make errors visible in the main left pane too
                           app.left_committed.push(msg);
                            if !app.current_response.trim().is_empty() {
                               app.left_committed.push(format!("Agent (partial): {}", app.current_response.trim()));
                               app.current_response.clear();
                            }
                          app.needs_redraw = true;
                          app.is_processing = false;
                          schedule_balance_refresh(&agent, &balance_tx);
                        }
                        UiUpdate::ApprovalRequested => {
                            // Poll the approval channel here to set pending immediately
                            while let Ok((desc, tx)) = approval_req_rx.try_recv() {
                               app.pending_approval = Some(desc);
                               app.approval_responder = Some(tx);
                               app.needs_redraw = true;
                               app.clear_input();
                            }
                            // Force a redraw so the dialog appears immediately
                          app.left_follow_output = true;
                          app.right_follow_output = true;
                        }
                        UiUpdate::ContextUsage { used_tokens } => {
                          app.ctx_used_tokens = used_tokens;
                        }
                        UiUpdate::InterjectRestart => {
                          app.current_response.clear();
                          app.current_thinking.clear();
                          app.needs_redraw = true;
                          app.left_follow_output = true;
                          app.left_scroll = 10_000;
                          app.right_follow_output = true;
                          app.right_scroll = 10_000;
                        }
                    }
                }

                // Allow the user to scroll AND type-ahead while processing
                Some(ev) = input_rx.recv() => {
                    match ev {
                    Event::Paste(text) => {
                        if app.settings.active {
                            app.paste_into_settings(&text);
                        } else {
                            app.paste_into_input(&text);
                        }
                    }
                    Event::Key(key) => {
                        // Highest priority: approval dialog
                        if app.handle_approval_key(key) {
                            continue;
                        }

                        if is_paste_key(&key) {
                            app.handle_clipboard_paste_key();
                            continue;
                        }

                        if app.settings.active {
                            apply_settings_key(&mut app, key, &config, &mut keystore, &agent, &balance_tx).await;
                            continue;
                        }

                        // Handle /mode selection menu
                        if app.handle_mode_menu_key(key, &agent).await {
                            continue;
                        }

                        match key.code {
                            KeyCode::Tab if key.modifiers.contains(KeyModifiers::SHIFT) => {
                                app.cycle_focus_backward();
                                app.needs_redraw = true;
                                continue;
                            }
                            KeyCode::Tab => {
                                app.cycle_focus_forward();
                                app.needs_redraw = true;
                                continue;
                            }
                            KeyCode::Esc => {
                                // STOP: signal the agent task to halt at the next clean point
                                stop_signal.store(true, Ordering::SeqCst);
                                // If there's a pending approval, deny it automatically
                                if let Some(tx) = app.approval_responder.take() {
                                    let _ = tx.send(false);
                                }
                               app.pending_approval = None;
                              app.trace_lines.push("⏹ Stop requested by user (Esc)".to_string());
                               app.left_committed.push("⏹ Stopping agent...".to_string());
                              app.left_follow_output = true;
                              app.left_scroll = 10_000;
                              app.right_follow_output = true;
                              app.right_scroll = 10_000;
                            }

                            KeyCode::PageUp if matches!(app.focused_pane, Pane::Left | Pane::Right) => {
                                app.scroll_focused_page(-1, 8);
                            }
                            KeyCode::PageDown if matches!(app.focused_pane, Pane::Left | Pane::Right) => {
                                app.scroll_focused_page(1, 8);
                            }
                            KeyCode::Up if matches!(app.focused_pane, Pane::Left | Pane::Right) => {
                                app.scroll_focused_line(-1);
                            }
                            KeyCode::Down if matches!(app.focused_pane, Pane::Left | Pane::Right) => {
                                app.scroll_focused_line(1);
                            }
                            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                return Ok(());
                            }

                            KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                app.insert_char('\n');
                                app.needs_redraw = true;
                            }
                            KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT)
                                || key.modifiers.contains(KeyModifiers::ALT) =>
                            {
                                app.insert_char('\n');
                                app.needs_redraw = true;
                            }
                            KeyCode::Enter if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                let text = app.input.clone();
                                app.submit_instant_interject(
                                    text,
                                    &queued_interject,
                                    &instant_interject,
                                    &stop_signal,
                                );
                            }
                            KeyCode::Enter if !app.input.trim().is_empty()
                                && !app.input.starts_with('/') =>
                            {
                                let text = app.input.clone();
                                app.submit_queued_interject(text, &queued_interject);
                            }

                            // Cursor movement during processing (desktop slide takes precedence)
                            KeyCode::Left if app.route_left_to_desktop() && app.try_slide_to_splash() => {}
                            KeyCode::Right if app.route_right_to_desktop() && app.try_slide_to_workspace() => {}
                            _ => {
                                if let Some(action) = map_key_to_edit(&key) {
                                    app.apply_edit_action(action);
                                    app.history_index = None;
                                    clamp_slash_selection(
                                        &app.slash_commands,
                                        &app.input,
                                        &mut app.slash_selected,
                                    );
                                    app.needs_redraw = true;
                                }
                            }
                        }
                    }
                    _ => {}
                    }
                }

                else => {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }
            continue;
        }

        // Normal input handling when idle — read from the input channel
        let input_ev = tokio::time::timeout(Duration::from_millis(50), input_rx.recv()).await;
        if let Ok(Some(ev)) = input_ev {
                match ev {
                Event::Paste(text) => {
                    if app.settings.active {
                        app.paste_into_settings(&text);
                    } else {
                        app.paste_into_input(&text);
                    }
                    continue;
                }
                Event::Key(key) => {
                if app.handle_approval_key(key) {
                    continue;
                }
                if app.handle_mode_menu_key(key, &agent).await {
                    continue;
                }
                if is_paste_key(&key) {
                    app.handle_clipboard_paste_key();
                    continue;
                }
                if app.settings.active {
                    apply_settings_key(&mut app, key, &config, &mut keystore, &agent, &balance_tx).await;
                    continue;
                }
                match key.code {
                    // --- Slash command menu navigation (takes precedence when active) ---
                    KeyCode::Up if app.focused_pane == Pane::Input && app.input.starts_with('/') => {
                        let filtered = get_filtered_commands(&app.slash_commands, &app.input);
                        if !filtered.is_empty() {
                           app.slash_selected = app.slash_selected.saturating_sub(1);
                        }
                       app.needs_redraw = true;
                    }
                    KeyCode::Down if app.focused_pane == Pane::Input && app.input.starts_with('/') => {
                        let filtered = get_filtered_commands(&app.slash_commands, &app.input);
                        if !filtered.is_empty() {
                           app.slash_selected = (app.slash_selected + 1).min(filtered.len().saturating_sub(1));
                        }
                       app.needs_redraw = true;
                    }
                    KeyCode::Tab if app.input.starts_with('/') => {
                        let filtered = get_filtered_commands(&app.slash_commands, &app.input);
                        if let Some(cmd) = filtered.get(app.slash_selected.min(filtered.len().saturating_sub(1))) {
                            app.set_input(format!("/{} ", cmd.name));
                            clamp_slash_selection(&app.slash_commands, &app.input, &mut app.slash_selected);
                        }
                    }

                    // --- Focus cycling ---
                    KeyCode::Tab if key.modifiers.contains(KeyModifiers::SHIFT) => {
                        app.cycle_focus_backward();
                        app.needs_redraw = true;
                    }
                    KeyCode::Tab => {
                        app.cycle_focus_forward();
                        app.needs_redraw = true;
                    }
                    KeyCode::Esc => {
                        if app.input.starts_with('/') {
                            // Dismiss command menu / clear partial command
                            app.clear_input();
                            app.slash_selected = 0;
                        } else if app.search_mode {
                            app.search_mode = false;
                            app.search.active = false;
                        } else {
                            // Reset focus to conversation pane, resume following
                            app.focused_pane = Pane::Left;
                            app.left_follow_output = true;
                            app.right_follow_output = true;
                        }
                    }

                    // --- Ctrl+C: exit ---
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        return Ok(());
                    }

                    // --- Multiline: Ctrl+J (reliable), Shift+Enter, or Alt+Enter inserts newline ---
                    // Most terminals can't distinguish Shift+Enter from Enter,
                    // but Ctrl+J (ASCII line feed) always works.
                    KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        app.insert_char('\n');
                        app.needs_redraw = true;
                    }
                    KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT)
                        || key.modifiers.contains(KeyModifiers::ALT) => {
                        app.insert_char('\n');
                        app.needs_redraw = true;
                    }

                    // --- Enter: submit prompt (works from any pane) ---
                    KeyCode::Enter => {
                        let prompt = app.input.trim().to_string();
                        if !prompt.is_empty() {
                            if app.input_history.last() != Some(&prompt) {
                                app.input_history.push(prompt.clone());
                            }
                            app.history_index = None;
                        }
                        if prompt.is_empty() {
                            // nothing to do
                        } else if prompt.starts_with('/') {
                            let mut slash_ctx = SlashContext {
                                left_committed: &mut app.left_committed,
                                trace_lines: &mut app.trace_lines,
                                current_response: &app.current_response,
                                current_thinking: &app.current_thinking,
                                input: &mut app.input,
                                cursor_pos: &mut app.cursor_pos,
                                slash_commands: &app.slash_commands,
                                slash_selected: &mut app.slash_selected,
                                mode_menu_active: &mut app.mode_menu_active,
                                selected_mode_idx: &mut app.selected_mode_idx,
                                settings: &mut app.settings,
                                search: &mut app.search,
                                focused_pane: render_pane(app.focused_pane),
                                left_scroll: &mut app.left_scroll,
                                right_scroll: &mut app.right_scroll,
                                left_follow_output: &mut app.left_follow_output,
                                right_follow_output: &mut app.right_follow_output,
                                last_left_line_count: app.last_left_line_count,
                                last_right_line_count: app.last_right_line_count,
                                last_left_area_h: app.last_left_area.height,
                                last_right_area_h: app.last_right_area.height,
                                config: &config,
                                keystore: &keystore,
                                agent: &agent,
                            };
                            match dispatch_slash_command(&prompt, &mut slash_ctx) {
                                SlashDispatch::Quit => return Ok(()),
                                SlashDispatch::Handled => {
                                    // /search sets pane scroll via apply_search_scroll; don't jump to bottom.
                                    if app.search.active
                                        && !app.search.query.is_empty()
                                        && !app.search.match_lines.is_empty()
                                    {
                                        app.needs_redraw = true;
                                    } else {
                                        app.left_follow_output = true;
                                        app.left_scroll = 10_000;
                                        app.needs_redraw = true;
                                    }
                                }
                                SlashDispatch::AgentPrompt(()) => {}
                            }
                            app.cursor_pos = app.input.len(); // reset cursor after slash clears input
                        } else {
                            // === Normal user prompt to the agent ===
                            // Commit any previous live response (from last turn) if present
                            // Guard against duplicate if Done already committed it.
                            if !app.current_response.trim().is_empty() {
                                let agent_msg = format!("Agent: {}", app.current_response.trim());
                                if app.left_committed.last() != Some(&agent_msg) {
                                    app.left_committed.push(agent_msg);
                                }
                            }

                            // New turn: record user prompt on left, clear live + trace for fresh view
                           app.left_committed.push(format!("You: {}", prompt));
                          app.left_follow_output = true;
                          app.left_scroll = 10_000; // auto-scroll to bottom on new user message
                           app.current_response.clear();
                          app.trace_lines.clear();
                          app.current_thinking.clear();
                          app.trace_lines.push(format!("▶ Starting agent turn for: {}", prompt));
                          app.trace_lines.push("   (waiting for first response from model...)".to_string());
                          app.right_follow_output = true;
                          app.right_scroll = 10_000; // auto-scroll trace pane on new turn start

                           app.clear_input();
                           app.slash_selected = 0;
                           app.is_processing = true;
                           app.tool_calls_this_turn = 0;
                           app.turn_rounds = 0;

                            // Spawn the agent turn — now using the unified drive_turn() via TuiObserver.
                            // The observer delivers UI events and handles live approvals + interjects.
                            // All nudges, auto-continue, finish_reason handling etc. come from one place.
                            let tx2 = tx.clone();
                            let agent_clone = agent.clone();
                            let prompt2 = prompt.clone();

                            let approval_req_tx2 = approval_req_tx.clone();
                            let stop = stop_signal.clone();
                            let queued = queued_interject.clone();
                            let instant = instant_interject.clone();
                            stop.store(false, Ordering::SeqCst); // reset for new turn
                            if let Ok(mut slot) = queued.lock() {
                                *slot = None;
                            }
                            if let Ok(mut slot) = instant.lock() {
                                *slot = None;
                            }
                            tokio::spawn(async move {
                                let mut agent = agent_clone.lock().await;

                                let mode = agent.current_exec_mode();
                                let _ = tx2.send(UiUpdate::ToolResult {
                                    name: "system".into(),
                                    summary: format!("Turn started with exec mode: {:?}", mode),
                                }).await;

                                let mut observer = TuiObserver {
                                    tx: tx2.clone(),
                                    approval_req_tx: approval_req_tx2,
                                    stop,
                                    queued,
                                    instant,
                                    denials_this_turn: 0,
                                    halt_tools: false,
                                    exec_mode: mode,
                                };

                                let result = crate::agent_driver::drive_turn(&mut agent, &prompt2, &mut observer).await;

                                match result {
                                    Ok(r) => {
                                        let _ = tx2.send(UiUpdate::Done { final_text: r.final_text }).await;
                                    }
                                    Err(e) => {
                                        let _ = tx2.send(UiUpdate::Error(e.to_string())).await;
                                    }
                                }
                            });
                        }
                    }
                    // --- History recall (Ctrl+Up / Ctrl+Down always works) ---
                    KeyCode::Up if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        if !app.input_history.is_empty() {
                            let idx = match app.history_index {
                                Some(i) if i > 0 => i - 1,
                                Some(i) => i,
                                None => app.input_history.len() - 1,
                            };
                            app.history_index = Some(idx);
                            app.set_input(app.input_history[idx].clone());
                            clamp_slash_selection(&app.slash_commands, &app.input, &mut app.slash_selected);
                            app.needs_redraw = true;
                        }
                    }
                    KeyCode::Down if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        if let Some(i) = app.history_index {
                            if i + 1 < app.input_history.len() {
                                let next = i + 1;
                                app.history_index = Some(next);
                                app.set_input(app.input_history[next].clone());
                            } else {
                                app.history_index = None;
                                app.clear_input();
                            }
                            clamp_slash_selection(&app.slash_commands, &app.input, &mut app.slash_selected);
                            app.needs_redraw = true;
                        }
                    }

                    // --- Desktop slide (pane focus) or cursor navigation (input) ---
                    KeyCode::Left
                        if !app.mode_menu_active
                            && app.pending_approval.is_none()
                            && app.route_left_to_desktop()
                            && app.try_slide_to_splash() => {}
                    KeyCode::Right
                        if !app.mode_menu_active
                            && app.pending_approval.is_none()
                            && app.route_right_to_desktop()
                            && app.try_slide_to_workspace() => {}

                    // --- Context-sensitive Up/Down ---
                    // When Input focused: history recall
                    KeyCode::Up if !app.mode_menu_active && app.focused_pane == Pane::Input => {
                        if !app.input_history.is_empty() {
                            let idx = match app.history_index {
                                Some(i) if i > 0 => i - 1,
                                Some(i) => i,
                                None => app.input_history.len() - 1,
                            };
                            app.history_index = Some(idx);
                            app.set_input(app.input_history[idx].clone());
                            clamp_slash_selection(&app.slash_commands, &app.input, &mut app.slash_selected);
                            app.needs_redraw = true;
                        }
                    }
                    KeyCode::Down if !app.mode_menu_active && app.focused_pane == Pane::Input => {
                        if let Some(i) = app.history_index {
                            if i + 1 < app.input_history.len() {
                                let next = i + 1;
                                app.history_index = Some(next);
                                app.set_input(app.input_history[next].clone());
                            } else {
                                app.history_index = None;
                                app.clear_input();
                            }
                            clamp_slash_selection(&app.slash_commands, &app.input, &mut app.slash_selected);
                            app.needs_redraw = true;
                        }
                    }

                    // Scroll focused pane (conversation or trace)
                    KeyCode::Up if !app.mode_menu_active => {
                        app.scroll_focused_line(-1);
                    }
                    KeyCode::Down if !app.mode_menu_active => {
                        app.scroll_focused_line(1);
                    }

                    // PageUp/PageDown: Shift scrolls trace pane, default scrolls conversation
                    KeyCode::PageUp if !app.mode_menu_active && key.modifiers.contains(KeyModifiers::SHIFT) => {
                        app.scroll_pane_page(Pane::Right, -1, 12);
                    }
                    KeyCode::PageUp if !app.mode_menu_active => {
                        app.scroll_pane_page(Pane::Left, -1, 12);
                    }
                    KeyCode::PageDown if !app.mode_menu_active && key.modifiers.contains(KeyModifiers::SHIFT) => {
                        app.scroll_pane_page(Pane::Right, 1, 12);
                    }
                    KeyCode::PageDown if !app.mode_menu_active => {
                        app.scroll_pane_page(Pane::Left, 1, 12);
                    }

                    // --- Text editing (always active, chars go to input from any pane) ---
                    _ if !app.mode_menu_active
                        && app.pending_approval.is_none()
                        && map_key_to_edit(&key).is_some() =>
                    {
                        if let Some(action) = map_key_to_edit(&key) {
                            app.apply_edit_action(action);
                            app.history_index = None;
                            clamp_slash_selection(
                                &app.slash_commands,
                                &app.input,
                                &mut app.slash_selected,
                            );
                            app.needs_redraw = true;
                        }
                    }

                    _ => {}
                }
                }
                _ => {}
                }
        }
    }
}

// StopAction, try_apply_queued_interject, and handle_stop_signal were removed
// as part of migrating the interactive loop to the unified drive_turn + TuiObserver.

enum UiUpdate {
    Token(String),
    Thinking(String),
    ToolStart { name: String, args: String },
    ToolResult { name: String, summary: String },
    RoundLimitHit { continuation: u32, max_continuations: u32, exhausted: bool },
    Done { final_text: String },
    Error(String),
    ApprovalRequested,  // wake the UI so it can display the dialog from the approval channel
    ContextUsage { used_tokens: u32 }, // pushed from agent task after tool rounds
    InterjectRestart,
}


/// Truncate a string for display, respecting UTF-8 char boundaries.
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut end = max;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &s[..end])
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// TuiObserver — wires the unified drive_turn() to the interactive TUI.
// All agentic policy lives in drive_turn; this only does UI events + live control.
// ─────────────────────────────────────────────────────────────────────────────

struct TuiObserver {
    tx: mpsc::Sender<UiUpdate>,
    approval_req_tx: mpsc::Sender<(String, oneshot::Sender<bool>)>,
    stop: Arc<AtomicBool>,
    queued: Arc<Mutex<Option<String>>>,
    instant: Arc<Mutex<Option<String>>>,
    denials_this_turn: u32,
    halt_tools: bool,
    exec_mode: ExecApprovalMode,
}

#[async_trait]
impl TurnObserver for TuiObserver {
    fn on_token(&mut self, t: &str) {
        let _ = self.tx.try_send(UiUpdate::Token(t.to_string()));
    }

    fn on_thinking(&mut self, t: &str) {
        let _ = self.tx.try_send(UiUpdate::Thinking(t.to_string()));
    }

    fn on_tool_start(&mut self, name: &str, args: &str) {
        let _ = self.tx.try_send(UiUpdate::ToolStart {
            name: name.to_string(),
            args: args.to_string(),
        });
    }

    fn on_tool_result(&mut self, record: &crate::agent::ActionRecord) {
        let summary = record.summary.clone();  // judge summaries already include ⭐ from driver
        let _ = self.tx.try_send(UiUpdate::ToolResult {
            name: record.tool.clone(),
            summary,
        });
    }

    fn on_nudge(&mut self, count: u32, max: u32) {
        let _ = self.tx.try_send(UiUpdate::ToolResult {
            name: "system".into(),
            summary: format!("Nudging agent to continue (text-only pause {}/{})", count, max),
        });
    }

    fn on_round_limit(&mut self, continuation: u32, max: u32, exhausted: bool) {
        let _ = self.tx.try_send(UiUpdate::RoundLimitHit {
            continuation,
            max_continuations: max,
            exhausted,
        });
    }

    fn on_stuck(&mut self, reason: &str, suggested: &str) {
        let _ = self.tx.try_send(UiUpdate::ToolResult {
            name: "system".into(),
            summary: format!("⚠ Agent looping: {}. Ask user: {}", reason, suggested),
        });
    }

    fn on_context_usage(&mut self, tokens: u32) {
        let _ = self.tx.try_send(UiUpdate::ContextUsage { used_tokens: tokens });
    }

    fn should_stop(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    fn on_interject(&mut self, _msg: &str) {
        let _ = self.tx.try_send(UiUpdate::InterjectRestart);
    }

    fn take_interject(&mut self) -> Option<String> {
        // Prefer instant interject (Esc + immediate new prompt)
        if let Ok(mut guard) = self.instant.lock() {
            if let Some(msg) = guard.take() {
                if !msg.trim().is_empty() {
                    self.stop.store(false, Ordering::SeqCst);
                    return Some(msg);
                }
            }
        }
        if let Ok(mut guard) = self.queued.lock() {
            if let Some(msg) = guard.take() {
                if !msg.trim().is_empty() {
                    self.stop.store(false, Ordering::SeqCst);
                    return Some(msg);
                }
            }
        }
        None
    }

    async fn approve_tool(&mut self, tc: &ToolCall) -> bool {
        let name = &tc.function.name;
        let args = &tc.function.arguments;

        if !self.needs_approval(name, args) {
            return true;
        }

        let desc = Self::build_approval_description(name, args);
        let (resp_tx, resp_rx) = oneshot::channel::<bool>();
        let _ = self.approval_req_tx.send((desc.clone(), resp_tx)).await;
        let _ = self.tx.send(UiUpdate::ApprovalRequested).await;

        // UI will show dialog and respond via the oneshot.
        // If approved, the caller (drive_turn) will emit on_tool_start right after.
        match resp_rx.await {
            Ok(true) => {
                true
            }
            _ => {
                let deny = format!(
                    "DENIED: The user refused to approve this {} action. \
                     Do NOT retry the same action. \
                     Either try a different approach, ask the user what they want, \
                     or explain what you were trying to do and why.",
                    name
                );
                self.denials_this_turn += 1;
                let _ = self.tx.send(UiUpdate::ToolResult {
                    name: name.clone(),
                    summary: deny.clone(),
                }).await;

                if self.denials_this_turn >= 3 {
                    let _ = self.tx.send(UiUpdate::ToolResult {
                        name: "system".into(),
                        summary: "3 actions denied this turn — stopping tool loop. Send a new message to continue.".into(),
                    }).await;
                    self.halt_tools = true;
                }
                false
            }
        }
    }

    fn stop_tool_processing(&self) -> bool {
        self.halt_tools
    }
}

impl TuiObserver {
    fn needs_approval(&self, name: &str, args: &str) -> bool {
        let is_mutating = matches!(name, "write" | "patch" | "exec");

        let is_outside = if name == "exec" {
            let cmd = serde_json::from_str::<serde_json::Value>(args)
                .ok()
                .and_then(|v| v.get("command").and_then(|c| c.as_str()).map(|s| s.to_owned()))
                .unwrap_or_default();
            cmd.contains("cd /")
                || cmd.contains("/etc")
                || cmd.contains("/root")
                || cmd.contains("curl ")
                || cmd.contains("wget ")
                || cmd.contains("nc ")
        } else {
            false
        };

        match self.exec_mode {
            ExecApprovalMode::Babysitter => is_mutating,
            ExecApprovalMode::SpringBreak => false,
            ExecApprovalMode::Vegas => name == "exec" && is_outside,
            ExecApprovalMode::Thunderdome => false,
        }
    }

    fn build_approval_description(name: &str, args: &str) -> String {
        match name {
            "write" => {
                let v: serde_json::Value = serde_json::from_str(args).unwrap_or_default();
                let path = v.get("path").and_then(|p| p.as_str()).unwrap_or("?");
                let n = v.get("content").and_then(|c| c.as_str()).map(|s| s.len()).unwrap_or(0);
                format!("write {} ({} bytes)", path, n)
            }
            "patch" => {
                let v: serde_json::Value = serde_json::from_str(args).unwrap_or_default();
                let path = v.get("path").and_then(|p| p.as_str()).unwrap_or("?");
                format!("patch {}", path)
            }
            "exec" => {
                let v: serde_json::Value = serde_json::from_str(args).unwrap_or_default();
                let cmd = v.get("command").and_then(|c| c.as_str()).unwrap_or("");
                let short = truncate(cmd, 72);
                format!("exec: {}", short)
            }
            "update_goal" => {
                let v: serde_json::Value = serde_json::from_str(args).unwrap_or_default();
                let goal = v.get("goal").and_then(|g| g.as_str()).map(|s| s.to_string())
                    .unwrap_or_else(|| args.to_string());
                format!("update_goal: {}", truncate(&goal, 80))
            }
            other => format!("{} (args omitted for display)", other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_ascii_short() {
        assert_eq!(truncate("hi", 80), "hi");
    }

    #[test]
    fn truncate_ascii_long() {
        assert_eq!(truncate("hello world", 5), "hello...");
    }

    #[test]
    fn truncate_multibyte_no_panic() {
        // "é" is 2 bytes; slicing at byte 1 would panic without boundary handling.
        let s = "éééééééééé"; // 20 bytes, 10 chars
        let out = truncate(s, 5);
        // Should back off to a char boundary (byte 4 = 2 chars) and append ellipsis.
        assert!(out.ends_with("..."));
        // 2 chars + "..." (3 chars) = 5 chars total.
        assert_eq!(out.chars().count(), 5);
    }

    #[test]
    fn truncate_emoji_boundary() {
        // "🦀" is 4 bytes. Truncating at byte 6 should back off to byte 4.
        let s = "🦀🦀🦀🦀🦀"; // 20 bytes
        let out = truncate(s, 6);
        assert!(out.ends_with("..."));
        // Should contain exactly one crab + ellipsis.
        assert_eq!(out, "🦀...");
    }
}
