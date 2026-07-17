//! Settings modal: endpoint list, add, edit, delete, switch, OpenRouter model browser.
//!
//! Extracted from `tui_app.rs` per glm.md refactor.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap},
    Frame,
};

use crate::key_edit::{map_key_to_edit, EditAction};
use crate::keystore::Keystore;
use raven_tui::agent::Agent;
use raven_tui::config::{Config, ContextBudget, InferenceEndpoint};
use raven_tui::llm::{self, is_openrouter, OpenRouterModelInfo};
use std::sync::Arc;
use tokio::sync::Mutex;

/// What the settings modal is currently doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsMode {
    List,
    Adding,
    Editing,
    BraveKey,
    /// Browse OpenRouter `/models` catalog and add/launch a model.
    OpenRouterBrowse,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OrFilter {
    All,
    Free,
    Cheap,
    LongCtx,
}

impl OrFilter {
    fn next(self) -> Self {
        match self {
            Self::All => Self::Free,
            Self::Free => Self::Cheap,
            Self::Cheap => Self::LongCtx,
            Self::LongCtx => Self::All,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Free => "free",
            Self::Cheap => "cheap",
            Self::LongCtx => "long-ctx",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OrSort {
    Name,
    Price,
    Context,
}

impl OrSort {
    fn next(self) -> Self {
        match self {
            Self::Name => Self::Price,
            Self::Price => Self::Context,
            Self::Context => Self::Name,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Price => "price",
            Self::Context => "context",
        }
    }
}

/// Mutable state for the inference-endpoints settings overlay.
#[derive(Clone, Debug)]
pub struct SettingsModal {
    pub active: bool,
    pub endpoints: Vec<InferenceEndpoint>,
    pub selected: usize,
    pub active_endpoint_idx: usize,
    pub mode: SettingsMode,
    /// Shared buffer for the active wizard field (add or edit).
    pub edit_buf: String,
    pub edit_cursor: usize,
    pub add_step: usize,
    pub new_label: String,
    pub new_url: String,
    pub new_model: String,
    pub new_key: String,
    /// List index being edited (`None` in list/add modes).
    pub editing_idx: Option<usize>,
    pub edit_step: usize,
    /// When editing API key, empty submit means "keep existing".
    pub edit_keep_key: bool,
    /// Whether a Brave Search API key is configured (for display in List mode).
    pub brave_key_configured: bool,

    // ── OpenRouter browser ─────────────────────────────────────────────
    pub or_models: Vec<OpenRouterModelInfo>,
    /// Indices into `or_models` after filter+sort.
    pub or_view: Vec<usize>,
    pub or_selected: usize,
    pub or_query: String,
    pub or_filter: OrFilter,
    pub or_sort: OrSort,
    pub or_status: String,
    pub or_loading: bool,
}

impl SettingsModal {
    pub fn inactive() -> Self {
        Self {
            active: false,
            endpoints: vec![],
            selected: 0,
            active_endpoint_idx: 0,
            mode: SettingsMode::List,
            edit_buf: String::new(),
            edit_cursor: 0,
            add_step: 0,
            new_label: String::new(),
            new_url: String::new(),
            new_model: String::new(),
            new_key: String::new(),
            editing_idx: None,
            edit_step: 0,
            edit_keep_key: false,
            brave_key_configured: false,
            or_models: vec![],
            or_view: vec![],
            or_selected: 0,
            or_query: String::new(),
            or_filter: OrFilter::Free,
            or_sort: OrSort::Name,
            or_status: String::new(),
            or_loading: false,
        }
    }

    /// First list entry: persisted launch defaults, or the agent's current config.
    pub fn session_list_head(keystore: &Keystore, fallback: &Config) -> InferenceEndpoint {
        keystore
            .launch_endpoint()
            .ok()
            .flatten()
            .unwrap_or_else(|| InferenceEndpoint::from_config(fallback))
    }

    pub fn open(&mut self, keystore: &Keystore, fallback_config: &Config) {
        let session_endpoint = Self::session_list_head(keystore, fallback_config);
        self.rebuild_endpoints(&session_endpoint, keystore);
        self.selected = self.active_endpoint_idx;
        self.mode = SettingsMode::List;
        self.editing_idx = None;
        self.clear_wizard();
        self.brave_key_configured = keystore.has_brave_key();
        self.active = true;
    }

    fn clear_wizard(&mut self) {
        self.edit_buf.clear();
        self.edit_cursor = 0;
        self.add_step = 0;
        self.edit_step = 0;
        self.new_label.clear();
        self.new_url.clear();
        self.new_model.clear();
        self.new_key.clear();
        self.editing_idx = None;
        self.edit_keep_key = false;
    }

    fn rebuild_endpoints(&mut self, session_endpoint: &InferenceEndpoint, keystore: &Keystore) {
        self.endpoints = vec![session_endpoint.clone()];
        if let Ok(eps) = keystore.decrypt_all_endpoints() {
            self.endpoints.extend(eps);
        }
        if self.selected >= self.endpoints.len() {
            self.selected = self.endpoints.len().saturating_sub(1);
        }
    }

    fn start_add(&mut self) {
        self.mode = SettingsMode::Adding;
        self.add_step = 0;
        self.edit_buf.clear();
        self.edit_cursor = 0;
        self.new_label.clear();
        self.new_url.clear();
        self.new_model.clear();
        self.new_key.clear();
    }

    fn start_edit(&mut self) {
        if self.endpoints.is_empty() {
            return;
        }
        let ep = self.endpoints[self.selected].clone();
        self.mode = SettingsMode::Editing;
        self.editing_idx = Some(self.selected);
        self.edit_step = 0;
        self.edit_buf = ep.label.clone();
        self.edit_cursor = self.edit_buf.len();
        self.new_label = ep.label;
        self.new_url = ep.base_url;
        self.new_model = ep.model;
        self.new_key.clear();
        self.edit_keep_key = ep.api_key.is_some();
    }

    fn clamp_edit_cursor(&mut self) {
        self.edit_cursor = self.edit_cursor.min(self.edit_buf.len());
    }

    fn insert_edit_char(&mut self, c: char) {
        self.clamp_edit_cursor();
        self.edit_buf.insert(self.edit_cursor, c);
        self.edit_cursor += c.len_utf8();
    }

    fn insert_edit_str(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        self.clamp_edit_cursor();
        self.edit_buf.insert_str(self.edit_cursor, s);
        self.edit_cursor += s.len();
    }

    pub fn apply_edit_action(&mut self, action: EditAction) {
        match action {
            EditAction::Insert(c) => self.insert_edit_char(c),
            EditAction::InsertStr(s) => self.insert_edit_str(&s),
            EditAction::Backspace => self.delete_edit_before(),
            EditAction::Delete => self.delete_edit_at(),
            EditAction::Left => self.move_edit_cursor_left(),
            EditAction::Right => self.move_edit_cursor_right(),
            EditAction::Home => self.move_edit_cursor_home(),
            EditAction::End => self.move_edit_cursor_end(),
        }
    }

    fn delete_edit_before(&mut self) {
        self.clamp_edit_cursor();
        if self.edit_cursor == 0 {
            return;
        }
        let prev = self.edit_buf[..self.edit_cursor]
            .char_indices()
            .next_back()
            .map(|(i, _)| i)
            .unwrap_or(0);
        self.edit_buf.drain(prev..self.edit_cursor);
        self.edit_cursor = prev;
    }

    fn delete_edit_at(&mut self) {
        self.clamp_edit_cursor();
        if self.edit_cursor >= self.edit_buf.len() {
            return;
        }
        let next = self.edit_cursor
            + self.edit_buf[self.edit_cursor..]
                .chars()
                .next()
                .map(|c| c.len_utf8())
                .unwrap_or(0);
        self.edit_buf.drain(self.edit_cursor..next);
    }

    fn move_edit_cursor_left(&mut self) {
        self.clamp_edit_cursor();
        if self.edit_cursor == 0 {
            return;
        }
        self.edit_cursor = self.edit_buf[..self.edit_cursor]
            .char_indices()
            .next_back()
            .map(|(i, _)| i)
            .unwrap_or(0);
    }

    fn move_edit_cursor_right(&mut self) {
        self.clamp_edit_cursor();
        if self.edit_cursor >= self.edit_buf.len() {
            return;
        }
        self.edit_cursor += self.edit_buf[self.edit_cursor..]
            .chars()
            .next()
            .map(|c| c.len_utf8())
            .unwrap_or(0);
    }

    fn move_edit_cursor_home(&mut self) {
        self.edit_cursor = 0;
    }

    fn move_edit_cursor_end(&mut self) {
        self.edit_cursor = self.edit_buf.len();
    }

    fn advance_add_field(&mut self) {
        match self.add_step {
            0 => {
                self.new_label = self.edit_buf.clone();
                self.edit_buf = "http://127.0.0.1:8080/v1".to_string();
                self.edit_cursor = self.edit_buf.len();
                self.add_step = 1;
            }
            1 => {
                self.new_url = self.edit_buf.clone();
                self.edit_buf.clear();
                self.edit_cursor = 0;
                self.add_step = 2;
            }
            2 => {
                self.new_model = self.edit_buf.clone();
                self.edit_buf.clear();
                self.edit_cursor = 0;
                self.add_step = 3;
            }
            _ => {}
        }
    }

    fn advance_edit_field(&mut self) {
        match self.edit_step {
            0 => {
                self.new_label = self.edit_buf.clone();
                self.edit_buf = self.new_url.clone();
                self.edit_cursor = self.edit_buf.len();
                self.edit_step = 1;
            }
            1 => {
                self.new_url = self.edit_buf.clone();
                self.edit_buf = self.new_model.clone();
                self.edit_cursor = self.edit_buf.len();
                self.edit_step = 2;
            }
            2 => {
                self.new_model = self.edit_buf.clone();
                self.edit_buf.clear();
                self.edit_cursor = 0;
                self.edit_step = 3;
            }
            _ => {}
        }
    }

    /// Rebuild `or_view` from query/filter/sort.
    pub fn rebuild_or_view(&mut self) {
        let q = self.or_query.to_ascii_lowercase();
        let mut idxs: Vec<usize> = self
            .or_models
            .iter()
            .enumerate()
            .filter(|(_, m)| {
                match self.or_filter {
                    OrFilter::All => true,
                    OrFilter::Free => m.is_free(),
                    OrFilter::Cheap => m.is_free() || m.prompt_per_million() <= 0.50,
                    OrFilter::LongCtx => m.context_length >= 100_000,
                }
            })
            .filter(|(_, m)| {
                if q.is_empty() {
                    return true;
                }
                m.id.to_ascii_lowercase().contains(&q)
                    || m.name.to_ascii_lowercase().contains(&q)
            })
            .map(|(i, _)| i)
            .collect();

        match self.or_sort {
            OrSort::Name => idxs.sort_by(|&a, &b| self.or_models[a].id.cmp(&self.or_models[b].id)),
            OrSort::Price => idxs.sort_by(|&a, &b| {
                self.or_models[a]
                    .prompt_per_million()
                    .partial_cmp(&self.or_models[b].prompt_per_million())
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| self.or_models[a].id.cmp(&self.or_models[b].id))
            }),
            OrSort::Context => idxs.sort_by(|&a, &b| {
                self.or_models[b]
                    .context_length
                    .cmp(&self.or_models[a].context_length)
                    .then_with(|| self.or_models[a].id.cmp(&self.or_models[b].id))
            }),
        }

        self.or_view = idxs;
        if self.or_selected >= self.or_view.len() {
            self.or_selected = self.or_view.len().saturating_sub(1);
        }
    }

    pub fn or_selected_model(&self) -> Option<&OpenRouterModelInfo> {
        self.or_view
            .get(self.or_selected)
            .and_then(|&i| self.or_models.get(i))
    }
}

/// Side effects produced by settings key handling.
pub enum SettingsAction {
    Redraw,
    Close,
    Notify(String),
    Trace(String),
    DisplayUpdate {
        model: String,
        budget: ContextBudget,
    },
    ActiveIdx(usize),
    /// Brave API key was updated — TUI should refresh agent.brave_key
    BraveKeyUpdated,
}

pub struct SettingsHandleResult {
    pub actions: Vec<SettingsAction>,
}

/// Draw the settings overlay.
pub fn draw_settings_modal(f: &mut Frame, area: Rect, settings: &SettingsModal) {
    if !settings.active {
        return;
    }

    let modal_w = 72u16.min(area.width.saturating_sub(2)).max(40);
    let modal_h = 28u16.min(area.height.saturating_sub(2)).max(14);
    let modal_x = (area.width.saturating_sub(modal_w)) / 2;
    let modal_y = (area.height.saturating_sub(modal_h)) / 2;
    let modal_area = Rect::new(modal_x, modal_y, modal_w, modal_h);

    f.render_widget(Clear, modal_area);

    let block = Block::default()
        .title(Span::styled(
            " Settings ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(modal_area);
    f.render_widget(block, modal_area);

    match settings.mode {
        SettingsMode::List => draw_list_mode(f, inner, settings),
        SettingsMode::OpenRouterBrowse => draw_or_browse_mode(f, inner, settings),
        SettingsMode::Adding | SettingsMode::Editing | SettingsMode::BraveKey => {
            draw_simple_mode(f, inner, settings);
        }
    }
}

fn draw_simple_mode(f: &mut Frame, area: Rect, settings: &SettingsModal) {
    let mut modal_lines = Text::default();
    match settings.mode {
        SettingsMode::Adding => {
            modal_lines.lines.push(Line::from(Span::styled(
                "  Add New Endpoint",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )));
            modal_lines.lines.push(Line::from(""));
            render_wizard_fields(
                &mut modal_lines,
                &["Label", "Base URL", "Model", "API Key (optional)"],
                &[
                    &settings.new_label,
                    &settings.new_url,
                    &settings.new_model,
                    &settings.new_key,
                ],
                settings.add_step,
                &settings.edit_buf,
                settings.edit_cursor,
            );
            modal_lines.lines.push(Line::from(""));
            if settings.add_step == 2
                && (is_openrouter(&settings.new_url) || is_openrouter(&settings.edit_buf))
            {
                modal_lines.lines.push(Line::from(Span::styled(
                    "  Tip: press B to browse OpenRouter models",
                    Style::default().fg(Color::Yellow),
                )));
            }
            modal_lines.lines.push(Line::from(Span::styled(
                "  ←→ move  •  Backspace  •  Ctrl+V paste  •  Enter next  •  Esc cancel",
                Style::default().fg(Color::DarkGray),
            )));
        }
        SettingsMode::Editing => {
            modal_lines.lines.push(Line::from(Span::styled(
                "  Edit Endpoint",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )));
            modal_lines.lines.push(Line::from(""));
            let key_display = if settings.edit_step > 3 {
                String::new()
            } else if !settings.new_key.is_empty() {
                "*".repeat(settings.new_key.len().min(20))
            } else if settings.edit_keep_key {
                "(keep existing)".to_string()
            } else {
                String::new()
            };
            render_wizard_fields(
                &mut modal_lines,
                &["Label", "Base URL", "Model", "API Key (blank=keep)"],
                &[
                    &settings.new_label,
                    &settings.new_url,
                    &settings.new_model,
                    &key_display,
                ],
                settings.edit_step,
                &settings.edit_buf,
                settings.edit_cursor,
            );
            modal_lines.lines.push(Line::from(""));
            if settings.edit_step == 2 && is_openrouter(&settings.new_url) {
                modal_lines.lines.push(Line::from(Span::styled(
                    "  Tip: press B to browse OpenRouter models",
                    Style::default().fg(Color::Yellow),
                )));
            }
            modal_lines.lines.push(Line::from(Span::styled(
                "  ←→ move  •  Backspace  •  Ctrl+V paste  •  Enter next  •  Esc cancel",
                Style::default().fg(Color::DarkGray),
            )));
        }
        SettingsMode::BraveKey => {
            modal_lines.lines.push(Line::from(Span::styled(
                "  Brave Search API Key",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )));
            modal_lines.lines.push(Line::from(""));
            modal_lines.lines.push(Line::from(Span::styled(
                "  Get a free key at https://brave.com/search/api/",
                Style::default().fg(Color::DarkGray),
            )));
            modal_lines.lines.push(Line::from(""));
            let cursor = settings.edit_cursor.min(settings.edit_buf.len());
            let before = settings.edit_buf[..cursor].to_string();
            let after = settings.edit_buf[cursor..].to_string();
            modal_lines.lines.push(Line::from(vec![
                Span::styled(
                    "  ▶ API Key: ",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(before, Style::default().fg(Color::White)),
                Span::styled(
                    "_",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::SLOW_BLINK),
                ),
                Span::styled(after, Style::default().fg(Color::White)),
            ]));
            modal_lines.lines.push(Line::from(""));
            if settings.brave_key_configured {
                modal_lines.lines.push(Line::from(Span::styled(
                    "  (currently set — submit empty to remove)",
                    Style::default().fg(Color::DarkGray),
                )));
            }
            modal_lines.lines.push(Line::from(Span::styled(
                "  Enter save  •  Esc cancel",
                Style::default().fg(Color::DarkGray),
            )));
        }
        _ => {}
    }
    f.render_widget(
        Paragraph::new(modal_lines).wrap(Wrap { trim: false }),
        area,
    );
}

/// List mode: fixed header + scrollable endpoints + fixed footer (A/E always visible).
fn draw_list_mode(f: &mut Frame, area: Rect, settings: &SettingsModal) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(4),
            Constraint::Length(5),
        ])
        .split(area);

    let header = Paragraph::new(Text::from(vec![
        Line::from(Span::styled(
            "  Inference Endpoints",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            format!(
                "  {} saved  ·  ↑↓ scroll  ·  list is scrollable",
                settings.endpoints.len()
            ),
            Style::default().fg(Color::DarkGray),
        )),
    ]));
    f.render_widget(header, chunks[0]);

    // ~3 lines per endpoint entry
    let list_h = chunks[1].height as usize;
    let rows_per = 3usize;
    let max_visible = (list_h / rows_per).max(1);
    let n = settings.endpoints.len();
    let scroll = {
        let sel = settings.selected.min(n.saturating_sub(1));
        if sel < max_visible {
            0
        } else {
            sel + 1 - max_visible
        }
    };
    let end = (scroll + max_visible).min(n);

    let mut list_lines = Text::default();
    if n == 0 {
        list_lines.lines.push(Line::from(Span::styled(
            "  (no endpoints)",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        if scroll > 0 {
            list_lines.lines.push(Line::from(Span::styled(
                format!("  ↑ {} more above", scroll),
                Style::default().fg(Color::DarkGray),
            )));
        }
        for i in scroll..end {
            let ep = &settings.endpoints[i];
            let is_sel = i == settings.selected;
            let is_active = i == settings.active_endpoint_idx;
            let marker = if is_active { "●" } else { "○" };
            let name_style = if is_sel {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            let sel_indicator = if is_sel { "▶ " } else { "  " };
            let free_badge = if ep.model.contains(":free") {
                "  free"
            } else {
                ""
            };

            list_lines.lines.push(Line::from(vec![
                Span::styled(
                    sel_indicator,
                    if is_sel {
                        Style::default().fg(Color::Cyan)
                    } else {
                        Style::default().fg(Color::Gray)
                    },
                ),
                Span::styled(
                    format!("{} ", marker),
                    if is_active {
                        Style::default().fg(Color::Green)
                    } else {
                        Style::default().fg(Color::DarkGray)
                    },
                ),
                Span::styled(ep.label.clone(), name_style),
                if is_active {
                    Span::styled("  [active]", Style::default().fg(Color::Green))
                } else {
                    Span::raw("")
                },
                Span::styled(free_badge, Style::default().fg(Color::Green)),
            ]));
            let url_disp = truncate_display(&ep.base_url, (chunks[1].width as usize).saturating_sub(8));
            list_lines.lines.push(Line::from(vec![
                Span::raw("      "),
                Span::styled(url_disp, Style::default().fg(Color::DarkGray)),
            ]));
            let key_indicator = if ep.api_key.is_some() { "  🔑" } else { "" };
            let model_disp =
                truncate_display(&ep.model, (chunks[1].width as usize).saturating_sub(16));
            list_lines.lines.push(Line::from(vec![
                Span::styled("      model: ", Style::default().fg(Color::DarkGray)),
                Span::styled(model_disp, Style::default().fg(Color::Gray)),
                Span::styled(key_indicator, Style::default().fg(Color::Yellow)),
            ]));
        }
        if end < n {
            list_lines.lines.push(Line::from(Span::styled(
                format!("  ↓ {} more below", n - end),
                Style::default().fg(Color::DarkGray),
            )));
        }
    }
    f.render_widget(
        Paragraph::new(list_lines).wrap(Wrap { trim: false }),
        chunks[1],
    );

    let brave_status = if settings.brave_key_configured {
        ("🔑 Brave: set", Color::Green)
    } else {
        ("Brave: not set", Color::DarkGray)
    };
    let has_or_key = find_openrouter_key(settings).is_some();
    let footer = Paragraph::new(Text::from(vec![
        Line::from(vec![
            Span::raw("  "),
            Span::styled(brave_status.0, Style::default().fg(brave_status.1)),
            Span::raw("  ·  "),
            if has_or_key {
                Span::styled("OpenRouter key ready", Style::default().fg(Color::Green))
            } else {
                Span::styled(
                    "OpenRouter: add key on an OR endpoint first",
                    Style::default().fg(Color::DarkGray),
                )
            },
        ]),
        Line::from(vec![
            Span::styled("  ↑↓ ", Style::default().fg(Color::DarkGray)),
            Span::styled("nav", Style::default().fg(Color::Gray)),
            Span::styled("  Enter ", Style::default().fg(Color::DarkGray)),
            Span::styled("switch", Style::default().fg(Color::Gray)),
            Span::styled(
                "  A ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("add", Style::default().fg(Color::Gray)),
            Span::styled(
                "  E ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("edit", Style::default().fg(Color::Gray)),
            Span::styled(
                "  D ",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Span::styled("del", Style::default().fg(Color::Gray)),
        ]),
        Line::from(vec![
            Span::styled(
                "  O ",
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("OpenRouter models", Style::default().fg(Color::Gray)),
            Span::styled(
                "  B ",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("brave key", Style::default().fg(Color::Gray)),
            Span::styled("  Esc ", Style::default().fg(Color::DarkGray)),
            Span::styled("close", Style::default().fg(Color::Gray)),
        ]),
    ]));
    f.render_widget(footer, chunks[2]);
}

fn draw_or_browse_mode(f: &mut Frame, area: Rect, settings: &SettingsModal) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Min(5),
            Constraint::Length(6),
        ])
        .split(area);

    let q = if settings.or_query.is_empty() {
        "█".to_string()
    } else {
        format!("{}█", settings.or_query)
    };
    let header = Paragraph::new(Text::from(vec![
        Line::from(Span::styled(
            "  OpenRouter models",
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            format!(
                "  filter:[{}]  sort:[{}]  {}",
                settings.or_filter.label(),
                settings.or_sort.label(),
                settings.or_status
            ),
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(vec![
            Span::styled("  Query: ", Style::default().fg(Color::DarkGray)),
            Span::styled(q, Style::default().fg(Color::White)),
        ]),
        Line::from(Span::styled(
            "  Tab filter  ·  Ctrl+S sort  ·  type to search  ·  Ctrl+R refresh",
            Style::default().fg(Color::DarkGray),
        )),
    ]));
    f.render_widget(header, chunks[0]);

    let list_h = chunks[1].height as usize;
    let max_visible = list_h.max(1);
    let n = settings.or_view.len();
    let scroll = {
        let sel = settings.or_selected.min(n.saturating_sub(1));
        if sel < max_visible {
            0
        } else {
            sel + 1 - max_visible
        }
    };
    let end = (scroll + max_visible).min(n);

    let mut list_lines = Text::default();
    if settings.or_loading {
        list_lines.lines.push(Line::from(Span::styled(
            "  Loading catalog…",
            Style::default().fg(Color::Yellow),
        )));
    } else if n == 0 {
        list_lines.lines.push(Line::from(Span::styled(
            "  No models match (try Tab → all, or clear query)",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        for vi in scroll..end {
            let mi = settings.or_view[vi];
            let m = &settings.or_models[mi];
            let is_sel = vi == settings.or_selected;
            let marker = if is_sel { "▶ " } else { "  " };
            let free = if m.is_free() { "free " } else { "     " };
            let ctx = if m.context_length > 0 {
                format!("{}k", m.context_length / 1000)
            } else {
                "?".into()
            };
            let id = truncate_display(&m.id, (chunks[1].width as usize).saturating_sub(22));
            let style = if is_sel {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            };
            list_lines.lines.push(Line::from(vec![
                Span::styled(marker, Style::default().fg(Color::Cyan)),
                Span::styled(
                    free,
                    if m.is_free() {
                        Style::default().fg(Color::Green)
                    } else {
                        Style::default().fg(Color::DarkGray)
                    },
                ),
                Span::styled(id, style),
                Span::styled(
                    format!("  {ctx}  {}", m.price_label()),
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
        }
    }
    f.render_widget(Paragraph::new(list_lines), chunks[1]);

    let preview = if let Some(m) = settings.or_selected_model() {
        vec![
            Line::from(Span::styled(
                format!("  {}", m.name),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                format!(
                    "  {}  ·  ctx {}  ·  {}",
                    m.id,
                    if m.context_length > 0 {
                        m.context_length.to_string()
                    } else {
                        "?".into()
                    },
                    m.price_label()
                ),
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(vec![
                Span::styled(
                    "  Enter ",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled("add endpoint  ", Style::default().fg(Color::Gray)),
                Span::styled(
                    "Ctrl+L ",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled("add+launch  ", Style::default().fg(Color::Gray)),
                Span::styled("Esc ", Style::default().fg(Color::DarkGray)),
                Span::styled("back", Style::default().fg(Color::Gray)),
            ]),
        ]
    } else {
        vec![Line::from(Span::styled(
            "  Select a model  ·  Esc back",
            Style::default().fg(Color::DarkGray),
        ))]
    };
    f.render_widget(Paragraph::new(Text::from(preview)), chunks[2]);
}

fn truncate_display(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if s.chars().count() <= max {
        return s.to_string();
    }
    let take = max.saturating_sub(1);
    let mut out: String = s.chars().take(take).collect();
    out.push('…');
    out
}

fn render_wizard_fields(
    modal_lines: &mut Text,
    fields: &[&str],
    values: &[&str],
    current_step: usize,
    edit_buf: &str,
    edit_cursor: usize,
) {
    for (i, (field, val)) in fields.iter().zip(values.iter()).enumerate() {
        let is_current = i == current_step;
        let marker = if i < current_step {
            "✓"
        } else if is_current {
            "▶"
        } else {
            " "
        };
        let label_style = if is_current {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else if i < current_step {
            Style::default().fg(Color::Green)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let display_val = if i == 3 && !val.is_empty() && val.chars().all(|c| c == '*') {
            val.to_string()
        } else if is_current {
            edit_buf.to_string()
        } else {
            val.to_string()
        };

        if is_current {
            let cursor = edit_cursor.min(display_val.len());
            let before = display_val[..cursor].to_string();
            let after = display_val[cursor..].to_string();
            modal_lines.lines.push(Line::from(vec![
                Span::styled(format!("  {} ", marker), label_style),
                Span::styled(format!("{}: ", field), label_style),
                Span::styled(before, Style::default().fg(Color::White)),
                Span::styled(
                    "_",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::SLOW_BLINK),
                ),
                Span::styled(after, Style::default().fg(Color::White)),
            ]));
        } else {
            modal_lines.lines.push(Line::from(vec![
                Span::styled(format!("  {} ", marker), label_style),
                Span::styled(format!("{}: ", field), label_style),
                Span::styled(display_val, Style::default().fg(Color::Gray)),
            ]));
        }
    }
}

fn find_openrouter_key(settings: &SettingsModal) -> Option<String> {
    for ep in &settings.endpoints {
        if is_openrouter(&ep.base_url) {
            if let Some(k) = ep.api_key.as_ref().filter(|k| !k.is_empty()) {
                return Some(k.clone());
            }
        }
    }
    std::env::var("OPENROUTER_API_KEY")
        .ok()
        .filter(|k| !k.is_empty())
}

/// Handle a key event while the settings modal is active.
pub async fn handle_settings_key(
    settings: &mut SettingsModal,
    key: KeyEvent,
    config: &Config,
    keystore: &mut Keystore,
    agent: &Arc<Mutex<Agent>>,
) -> SettingsHandleResult {
    // Ignore key release / other kinds — on many terminals Esc generates
    // Press then Release; handling both would pop Browse→List then List→close
    // in one physical keypress (flash of parent modal).
    if !matches!(
        key.kind,
        crossterm::event::KeyEventKind::Press | crossterm::event::KeyEventKind::Repeat
    ) {
        return SettingsHandleResult { actions: vec![] };
    }

    let mut actions = vec![SettingsAction::Redraw];

    match settings.mode {
        SettingsMode::Adding | SettingsMode::Editing => {
            // Browse OpenRouter from model step
            if matches!(key.code, KeyCode::Char('b') | KeyCode::Char('B'))
                && ((settings.mode == SettingsMode::Adding && settings.add_step == 2)
                    || (settings.mode == SettingsMode::Editing && settings.edit_step == 2))
                && (is_openrouter(&settings.new_url) || find_openrouter_key(settings).is_some())
            {
                actions.extend(open_openrouter_browse(settings).await);
                return SettingsHandleResult { actions };
            }
            if let Some(action) = map_key_to_edit(&key) {
                settings.apply_edit_action(action);
            } else {
                match key.code {
                    KeyCode::Enter => {
                        if settings.mode == SettingsMode::Adding {
                            if settings.add_step == 1 {
                                let url = settings.edit_buf.clone();
                                settings.new_url = url.clone();
                                settings.add_step = 2;
                                if is_openrouter(&url)
                                    && (find_openrouter_key(settings).is_some()
                                        || !settings.new_key.is_empty())
                                {
                                    actions.push(SettingsAction::Trace(
                                        "   ↳ OpenRouter URL — press B to browse models, or type a slug"
                                            .into(),
                                    ));
                                }
                                let key_for_probe = find_openrouter_key(settings);
                                match llm::probe_server(
                                    &url,
                                    "",
                                    key_for_probe.as_deref(),
                                )
                                .await
                                {
                                    Some(probe) => {
                                        let model_id = probe.model_id.clone();
                                        settings.new_model = model_id.clone();
                                        settings.edit_buf = model_id.clone();
                                        settings.edit_cursor = settings.edit_buf.len();
                                        actions.push(SettingsAction::Trace(format!(
                                            "   ↳ detected: {} ({} tokens)",
                                            model_id, probe.context_tokens
                                        )));
                                    }
                                    None => {
                                        settings.edit_buf.clear();
                                        settings.edit_cursor = 0;
                                        actions.push(SettingsAction::Trace(
                                            "   ↳ could not probe /v1/models — enter model or press B to browse"
                                                .into(),
                                        ));
                                    }
                                }
                            } else if settings.add_step < 3 {
                                settings.advance_add_field();
                            } else {
                                settings.new_key = settings.edit_buf.clone();
                                actions.extend(finish_add(settings, config, keystore));
                            }
                        } else if settings.mode == SettingsMode::Editing && settings.edit_step == 1 {
                            let url = settings.edit_buf.clone();
                            settings.new_url = url.clone();
                            let api_key = if settings.new_key.is_empty() {
                                settings
                                    .endpoints
                                    .get(settings.editing_idx.unwrap_or(0))
                                    .and_then(|ep| ep.api_key.as_deref())
                            } else {
                                Some(settings.new_key.as_str())
                            };
                            settings.edit_step = 2;
                            let hint = settings.new_model.as_str();
                            match llm::probe_server(&url, hint, api_key).await {
                                Some(probe) => {
                                    let model_id = probe.model_id.clone();
                                    settings.new_model = model_id.clone();
                                    settings.edit_buf = model_id.clone();
                                    settings.edit_cursor = settings.edit_buf.len();
                                    actions.push(SettingsAction::Trace(format!(
                                        "   ↳ detected: {} ({} tokens)",
                                        model_id, probe.context_tokens
                                    )));
                                }
                                None => {
                                    settings.edit_buf = settings.new_model.clone();
                                    settings.edit_cursor = settings.edit_buf.len();
                                    actions.push(SettingsAction::Trace(
                                        "   ↳ could not probe /v1/models — edit model or press B"
                                            .into(),
                                    ));
                                }
                            }
                        } else if settings.edit_step < 3 {
                            settings.advance_edit_field();
                        } else {
                            settings.new_key = settings.edit_buf.clone();
                            actions.extend(finish_edit(settings, config, keystore, agent).await);
                        }
                    }
                    KeyCode::Esc => {
                        settings.mode = SettingsMode::List;
                        settings.clear_wizard();
                    }
                    _ => {}
                }
            }
        }
        SettingsMode::List => match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                if settings.selected > 0 {
                    settings.selected -= 1;
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if settings.selected < settings.endpoints.len().saturating_sub(1) {
                    settings.selected += 1;
                }
            }
            KeyCode::PageUp => {
                settings.selected = settings.selected.saturating_sub(5);
            }
            KeyCode::PageDown => {
                let max = settings.endpoints.len().saturating_sub(1);
                settings.selected = (settings.selected + 5).min(max);
            }
            KeyCode::Enter => {
                if settings.selected != settings.active_endpoint_idx {
                    actions.extend(
                        switch_to_endpoint(settings, config, agent, keystore, settings.selected).await,
                    );
                }
                settings.active = false;
                actions.push(SettingsAction::Close);
            }
            KeyCode::Char('a') | KeyCode::Char('A') => {
                settings.start_add();
            }
            KeyCode::Char('e') | KeyCode::Char('E') => {
                settings.start_edit();
            }
            KeyCode::Char('o') | KeyCode::Char('O') => {
                actions.extend(open_openrouter_browse(settings).await);
            }
            KeyCode::Char('d') | KeyCode::Char('D') => {
                if settings.selected > 0 && settings.selected != settings.active_endpoint_idx {
                    let keystore_idx = settings.selected - 1;
                    let _ = keystore.remove_endpoint(keystore_idx);
                    let fallback = if let Ok(ag) = agent.try_lock() {
                        ag.current_config().clone()
                    } else {
                        config.clone()
                    };
                    let session_endpoint = SettingsModal::session_list_head(keystore, &fallback);
                    settings.rebuild_endpoints(&session_endpoint, keystore);
                    if settings.active_endpoint_idx > settings.selected {
                        settings.active_endpoint_idx -= 1;
                    }
                    actions.push(SettingsAction::Notify("Endpoint deleted.".into()));
                }
            }
            KeyCode::Char('b') | KeyCode::Char('B') => {
                settings.mode = SettingsMode::BraveKey;
                settings.edit_buf.clear();
                settings.edit_cursor = 0;
            }
            KeyCode::Esc => {
                settings.active = false;
                actions.push(SettingsAction::Close);
            }
            _ => {}
        },
        SettingsMode::OpenRouterBrowse => {
            actions.extend(handle_or_browse_key(settings, key, config, keystore, agent).await);
        }
        SettingsMode::BraveKey => {
            if let Some(action) = map_key_to_edit(&key) {
                settings.apply_edit_action(action);
            } else {
                match key.code {
                    KeyCode::Enter => {
                        let key_text = settings.edit_buf.trim().to_string();
                        if key_text.is_empty() {
                            if let Err(e) = keystore.clear_brave_key() {
                                actions.push(SettingsAction::Notify(format!(
                                    "Failed to clear Brave key: {}",
                                    e
                                )));
                            } else {
                                actions.push(SettingsAction::Notify(
                                    "Brave Search key removed.".into(),
                                ));
                                actions.push(SettingsAction::BraveKeyUpdated);
                            }
                        } else {
                            if !keystore.is_unlocked() {
                                actions.push(SettingsAction::Notify(
                                    "Setting vault password (first key). Using 'raven' as default."
                                        .into(),
                                ));
                                let _ = keystore.init_password("raven");
                            }
                            match keystore.set_brave_key(&key_text) {
                                Ok(()) => {
                                    actions.push(SettingsAction::Notify(
                                        "✅ Brave Search API key saved.".into(),
                                    ));
                                    actions.push(SettingsAction::BraveKeyUpdated);
                                }
                                Err(e) => {
                                    actions.push(SettingsAction::Notify(format!(
                                        "Failed to save Brave key: {}",
                                        e
                                    )));
                                }
                            }
                        }
                        settings.mode = SettingsMode::List;
                        settings.clear_wizard();
                    }
                    KeyCode::Esc => {
                        settings.mode = SettingsMode::List;
                        settings.clear_wizard();
                    }
                    _ => {}
                }
            }
        }
    }

    SettingsHandleResult { actions }
}

async fn open_openrouter_browse(settings: &mut SettingsModal) -> Vec<SettingsAction> {
    let mut actions = vec![];
    let Some(api_key) = find_openrouter_key(settings) else {
        actions.push(SettingsAction::Notify(
            "No OpenRouter API key found. Add an OpenRouter endpoint with a key first (or set OPENROUTER_API_KEY)."
                .into(),
        ));
        return actions;
    };

    settings.mode = SettingsMode::OpenRouterBrowse;
    settings.or_loading = true;
    settings.or_status = "loading…".into();
    settings.or_query.clear();
    settings.or_selected = 0;
    settings.or_filter = OrFilter::Free;
    settings.or_sort = OrSort::Name;
    settings.or_models.clear();
    settings.or_view.clear();

    match llm::fetch_openrouter_models(&api_key).await {
        Ok(models) => {
            let n = models.len();
            let free_n = models.iter().filter(|m| m.is_free()).count();
            settings.or_models = models;
            settings.or_loading = false;
            settings.rebuild_or_view();
            settings.or_status = format!("{n} total · {} free shown", settings.or_view.len());
            actions.push(SettingsAction::Notify(format!(
                "OpenRouter catalog: {n} models ({free_n} free). Tab filters free/cheap/all."
            )));
            actions.push(SettingsAction::Trace(format!(
                "   ↳ OpenRouter catalog loaded ({n} models)"
            )));
        }
        Err(e) => {
            settings.or_loading = false;
            settings.or_status = format!("error: {e}");
            actions.push(SettingsAction::Notify(format!(
                "Failed to load OpenRouter models: {e}"
            )));
        }
    }
    actions
}

async fn handle_or_browse_key(
    settings: &mut SettingsModal,
    key: KeyEvent,
    config: &Config,
    keystore: &mut Keystore,
    agent: &Arc<Mutex<Agent>>,
) -> Vec<SettingsAction> {
    let mut actions = vec![];

    // Ctrl+U clear query
    if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('u')) {
        settings.or_query.clear();
        settings.rebuild_or_view();
        return actions;
    }

    match key.code {
        KeyCode::Esc => {
            settings.mode = SettingsMode::List;
            settings.or_loading = false;
        }
        KeyCode::Up => {
            if settings.or_selected > 0 {
                settings.or_selected -= 1;
            }
        }
        KeyCode::Down => {
            if settings.or_selected + 1 < settings.or_view.len() {
                settings.or_selected += 1;
            }
        }
        KeyCode::PageUp => {
            settings.or_selected = settings.or_selected.saturating_sub(10);
        }
        KeyCode::PageDown => {
            let max = settings.or_view.len().saturating_sub(1);
            settings.or_selected = (settings.or_selected + 10).min(max);
        }
        KeyCode::Tab => {
            settings.or_filter = settings.or_filter.next();
            settings.rebuild_or_view();
            settings.or_status = format!(
                "{} shown · filter {}",
                settings.or_view.len(),
                settings.or_filter.label()
            );
        }
        KeyCode::Char('s') | KeyCode::Char('S') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            settings.or_sort = settings.or_sort.next();
            settings.rebuild_or_view();
            settings.or_status = format!(
                "{} shown · sort {}",
                settings.or_view.len(),
                settings.or_sort.label()
            );
        }
        KeyCode::Char('r') | KeyCode::Char('R') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            actions.extend(open_openrouter_browse(settings).await);
        }
        KeyCode::Char('l') | KeyCode::Char('L') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            if let Some(m) = settings.or_selected_model().cloned() {
                actions.extend(
                    add_openrouter_model(settings, config, keystore, &m, true, agent).await,
                );
            }
        }
        KeyCode::Enter => {
            if let Some(m) = settings.or_selected_model().cloned() {
                actions.extend(
                    add_openrouter_model(settings, config, keystore, &m, false, agent).await,
                );
            }
        }
        KeyCode::Backspace => {
            settings.or_query.pop();
            settings.rebuild_or_view();
        }
        KeyCode::Char(c)
            if !key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT)
                && (c.is_ascii_graphic() || c == ' ') =>
        {
            settings.or_query.push(c);
            settings.rebuild_or_view();
        }
        _ => {}
    }
    actions
}

async fn add_openrouter_model(
    settings: &mut SettingsModal,
    config: &Config,
    keystore: &mut Keystore,
    model: &OpenRouterModelInfo,
    launch: bool,
    agent: &Arc<Mutex<Agent>>,
) -> Vec<SettingsAction> {
    let mut actions = vec![];
    let Some(api_key) = find_openrouter_key(settings) else {
        actions.push(SettingsAction::Notify(
            "No OpenRouter API key available.".into(),
        ));
        return actions;
    };

    let label = format!(
        "OR · {}",
        if model.is_free() {
            format!("{} (free)", model.short_label())
        } else {
            model.short_label()
        }
    );
    let url = "https://openrouter.ai/api/v1";

    if !keystore.is_unlocked() {
        let _ = keystore.init_password("raven");
    }

    match keystore.add_endpoint(&label, url, &model.id, Some(api_key.as_str())) {
        Ok(()) => {
            actions.push(SettingsAction::Notify(format!("Added {}", model.id)));
            actions.push(SettingsAction::Trace(format!(
                "   ↳ saved endpoint {} → {}",
                label, model.id
            )));
        }
        Err(e) => {
            actions.push(SettingsAction::Notify(format!("Failed to save: {e}")));
            return actions;
        }
    }

    let session_endpoint = SettingsModal::session_list_head(keystore, config);
    settings.rebuild_endpoints(&session_endpoint, keystore);

    // Select the newly added endpoint (last in keystore list → last in endpoints after head)
    let new_idx = settings.endpoints.len().saturating_sub(1);
    settings.selected = new_idx;

    if launch {
        actions.extend(switch_to_endpoint(settings, config, agent, keystore, new_idx).await);
        settings.mode = SettingsMode::List;
        actions.push(SettingsAction::Notify(format!(
            "Launching {}",
            model.id
        )));
    } else {
        settings.mode = SettingsMode::List;
    }
    actions
}

fn finish_add(
    settings: &mut SettingsModal,
    config: &Config,
    keystore: &mut Keystore,
) -> Vec<SettingsAction> {
    let mut actions = vec![];

    let api_key_opt = if settings.new_key.is_empty() {
        // Reuse OpenRouter key when adding an OpenRouter URL without typing key again
        if is_openrouter(&settings.new_url) {
            find_openrouter_key(settings)
        } else {
            None
        }
    } else {
        if !keystore.is_unlocked() {
            actions.push(SettingsAction::Notify(
                "Setting vault password (first API key). Using 'raven' as default.".into(),
            ));
            let _ = keystore.init_password("raven");
        }
        Some(settings.new_key.clone())
    };

    match keystore.add_endpoint(
        &settings.new_label,
        &settings.new_url,
        &settings.new_model,
        api_key_opt.as_deref(),
    ) {
        Ok(()) => actions.push(SettingsAction::Notify(format!(
            "Added endpoint: {}",
            settings.new_label
        ))),
        Err(e) => actions.push(SettingsAction::Notify(format!(
            "Failed to save endpoint: {}",
            e
        ))),
    }

    let session_endpoint = SettingsModal::session_list_head(keystore, config);
    settings.rebuild_endpoints(&session_endpoint, keystore);
    settings.mode = SettingsMode::List;
    settings.clear_wizard();
    actions
}

async fn finish_edit(
    settings: &mut SettingsModal,
    config: &Config,
    keystore: &mut Keystore,
    agent: &Arc<Mutex<Agent>>,
) -> Vec<SettingsAction> {
    let mut actions = vec![];

    let Some(list_idx) = settings.editing_idx else {
        settings.mode = SettingsMode::List;
        settings.clear_wizard();
        return actions;
    };

    if list_idx == 0 {
        let prev = &settings.endpoints[0];
        let key_update = if settings.new_key.is_empty() {
            None
        } else {
            if !keystore.is_unlocked() {
                actions.push(SettingsAction::Notify(
                    "Setting vault password (first API key). Using 'raven' as default.".into(),
                ));
                let _ = keystore.init_password("raven");
            }
            Some(settings.new_key.as_str())
        };

        let saved = keystore.set_launch_defaults(
            &settings.new_label,
            &settings.new_url,
            &settings.new_model,
            key_update,
        );
        let api_key = if settings.new_key.is_empty() {
            prev.api_key.clone()
        } else {
            Some(settings.new_key.clone())
        };
        let updated = InferenceEndpoint {
            label: settings.new_label.clone(),
            base_url: settings.new_url.clone(),
            model: settings.new_model.clone(),
            api_key,
        };

        match saved {
            Ok(()) => {
                actions.push(SettingsAction::Notify(format!(
                    "Updated launch endpoint: {} (saved to endpoints.json)",
                    settings.new_label
                )));
                settings.rebuild_endpoints(&updated, keystore);
                if list_idx == settings.active_endpoint_idx {
                    actions.extend(switch_to_endpoint(settings, config, agent, keystore, 0).await);
                }
            }
            Err(e) => actions.push(SettingsAction::Notify(format!(
                "Failed to save launch endpoint: {}",
                e
            ))),
        }
    } else {
        let keystore_idx = list_idx - 1;
        let key_update = if settings.new_key.is_empty() {
            None
        } else {
            if !keystore.is_unlocked() {
                actions.push(SettingsAction::Notify(
                    "Setting vault password (first API key). Using 'raven' as default.".into(),
                ));
                let _ = keystore.init_password("raven");
            }
            Some(settings.new_key.as_str())
        };

        match keystore.update_endpoint(
            keystore_idx,
            &settings.new_label,
            &settings.new_url,
            &settings.new_model,
            key_update,
        ) {
            Ok(()) => {
                actions.push(SettingsAction::Notify(format!(
                    "Updated endpoint: {}",
                    settings.new_label
                )));
                let session_endpoint = SettingsModal::session_list_head(keystore, config);
                settings.rebuild_endpoints(&session_endpoint, keystore);
                if list_idx == settings.active_endpoint_idx {
                    actions.extend(switch_to_endpoint(settings, config, agent, keystore, list_idx).await);
                }
            }
            Err(e) => actions.push(SettingsAction::Notify(format!(
                "Failed to update endpoint: {}",
                e
            ))),
        }
    }

    settings.mode = SettingsMode::List;
    settings.clear_wizard();
    actions
}

async fn switch_to_endpoint(
    settings: &SettingsModal,
    config: &Config,
    agent: &Arc<Mutex<Agent>>,
    keystore: &mut Keystore,
    idx: usize,
) -> Vec<SettingsAction> {
    let mut actions = vec![];
    let Some(ep) = settings.endpoints.get(idx) else {
        return actions;
    };

    actions.push(SettingsAction::Trace(format!(
        "⟳ Switching to: {} ({})",
        ep.label, ep.base_url
    )));

    // Persist the selected endpoint as the launch default so it survives restarts.
    if let Err(e) = keystore.set_launch_endpoint(ep) {
        actions.push(SettingsAction::Notify(format!(
            "Warning: failed to persist endpoint: {}",
            e
        )));
    }

    let mut active = ep.clone();
    let budget =
        match llm::probe_server(&ep.base_url, &ep.model, ep.api_key.as_deref()).await {
            Some(probe) => {
                active.model = probe.model_id.clone();
                if probe.model_id != ep.model {
                    actions.push(SettingsAction::Trace(format!(
                        "   ↳ model: {} (configured: {})",
                        probe.model_id, ep.model
                    )));
                }
                actions.push(SettingsAction::Trace(format!(
                    "   ↳ context: {} tokens (probed)",
                    probe.context_tokens
                )));
                ContextBudget::from_context_tokens(probe.context_tokens, config.max_rounds)
            }
            None => {
                actions.push(SettingsAction::Trace(
                    "   ↳ probe failed, using default 8192".into(),
                ));
                ContextBudget::default_fallback()
            }
        };

    match agent.try_lock() {
        Ok(mut ag) => {
            ag.switch_endpoint(&active, budget.clone()).await;
            actions.push(SettingsAction::DisplayUpdate {
                model: active.model.clone(),
                budget: budget.clone(),
            });
            actions.push(SettingsAction::ActiveIdx(idx));
            actions.push(SettingsAction::Notify(format!(
                "Switched to: {} ({})",
                ep.label, active.model
            )));
        }
        Err(_) => {
            actions.push(SettingsAction::Notify(
                "Agent busy — could not switch endpoint now.".into(),
            ));
        }
    }
    actions
}
