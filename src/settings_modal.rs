//! Settings modal: endpoint list, add, edit, delete, and switch.
//!
//! Extracted from `tui_app.rs` per glm.md refactor.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap},
    Frame,
};

use crate::agent::Agent;
use crate::config::{Config, ContextBudget, InferenceEndpoint};
use crate::key_edit::{map_key_to_edit, EditAction};
use crate::keystore::Keystore;
use std::sync::Arc;
use tokio::sync::Mutex;

/// What the settings modal is currently doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsMode {
    List,
    Adding,
    Editing,
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
}

/// Side effects produced by settings key handling.
pub enum SettingsAction {
    Redraw,
    Close,
    Notify(String),
    Trace(String),
    DisplayUpdate { model: String, budget: ContextBudget },
    ActiveIdx(usize),
}

pub struct SettingsHandleResult {
    pub actions: Vec<SettingsAction>,
}

/// Draw the settings overlay.
pub fn draw_settings_modal(f: &mut Frame, area: Rect, settings: &SettingsModal) {
    if !settings.active {
        return;
    }

    let modal_w = 64u16.min(area.width.saturating_sub(4));
    let modal_h = 24u16.min(area.height.saturating_sub(4));
    let modal_x = (area.width.saturating_sub(modal_w)) / 2;
    let modal_y = (area.height.saturating_sub(modal_h)) / 2;
    let modal_area = Rect::new(modal_x, modal_y, modal_w, modal_h);

    let mut modal_lines = Text::default();

    match settings.mode {
        SettingsMode::Adding => {
            modal_lines.lines.push(Line::from(Span::styled(
                "  Add New Endpoint",
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            )));
            modal_lines.lines.push(Line::from(""));
            render_wizard_fields(
                &mut modal_lines,
                &["Label", "Base URL", "Model", "API Key (optional)"],
                &[&settings.new_label, &settings.new_url, &settings.new_model, &settings.new_key],
                settings.add_step,
                &settings.edit_buf,
                settings.edit_cursor,
            );
            modal_lines.lines.push(Line::from(""));
            modal_lines.lines.push(Line::from(Span::styled(
                "  ←→ move  •  Backspace edit  •  Ctrl+V / Shift+Insert paste  •  Enter next  •  Esc cancel",
                Style::default().fg(Color::DarkGray),
            )));
        }
        SettingsMode::Editing => {
            modal_lines.lines.push(Line::from(Span::styled(
                "  Edit Endpoint",
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
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
                &[&settings.new_label, &settings.new_url, &settings.new_model, &key_display],
                settings.edit_step,
                &settings.edit_buf,
                settings.edit_cursor,
            );
            modal_lines.lines.push(Line::from(""));
            modal_lines.lines.push(Line::from(Span::styled(
                "  ←→ move  •  Backspace edit  •  Ctrl+V / Shift+Insert paste  •  Enter next  •  Esc cancel",
                Style::default().fg(Color::DarkGray),
            )));
        }
        SettingsMode::List => {
            modal_lines.lines.push(Line::from(Span::styled(
                "  Inference Endpoints",
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            )));
            modal_lines.lines.push(Line::from(""));

            for (i, ep) in settings.endpoints.iter().enumerate() {
                let is_sel = i == settings.selected;
                let is_active = i == settings.active_endpoint_idx;
                let marker = if is_active { "●" } else { "○" };
                let name_style = if is_sel {
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
                };
                let sel_indicator = if is_sel { "▶ " } else { "  " };

                modal_lines.lines.push(Line::from(vec![
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
                    Span::styled(&ep.label, name_style),
                    if is_active {
                        Span::styled("  [active]", Style::default().fg(Color::Green))
                    } else {
                        Span::styled("", Style::default())
                    },
                ]));
                modal_lines.lines.push(Line::from(vec![
                    Span::styled("      ", Style::default()),
                    Span::styled(&ep.base_url, Style::default().fg(Color::DarkGray)),
                ]));
                let key_indicator = if ep.api_key.is_some() { "  🔑" } else { "" };
                modal_lines.lines.push(Line::from(vec![
                    Span::styled("      model: ", Style::default().fg(Color::DarkGray)),
                    Span::styled(&ep.model, Style::default().fg(Color::Gray)),
                    Span::styled(key_indicator, Style::default().fg(Color::Yellow)),
                ]));
                if i < settings.endpoints.len() - 1 {
                    modal_lines.lines.push(Line::from(""));
                }
            }

            modal_lines.lines.push(Line::from(""));
            modal_lines.lines.push(Line::from(vec![
                Span::styled("  ↑↓ ", Style::default().fg(Color::DarkGray)),
                Span::styled("navigate", Style::default().fg(Color::Gray)),
                Span::styled("  Enter ", Style::default().fg(Color::DarkGray)),
                Span::styled("switch", Style::default().fg(Color::Gray)),
                Span::styled("  A ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                Span::styled("add", Style::default().fg(Color::Gray)),
                Span::styled("  E ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                Span::styled("edit", Style::default().fg(Color::Gray)),
            ]));
            modal_lines.lines.push(Line::from(vec![
                Span::styled("  D ", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
                Span::styled("delete", Style::default().fg(Color::Gray)),
                Span::styled("  Esc ", Style::default().fg(Color::DarkGray)),
                Span::styled("close", Style::default().fg(Color::Gray)),
            ]));
        }
    }

    let modal_block = Paragraph::new(modal_lines)
        .wrap(Wrap { trim: false })
        .block(
            Block::default()
                .title(Span::styled(
                    " Settings ",
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                ))
                .borders(Borders::ALL)
                .border_type(BorderType::Double)
                .border_style(Style::default().fg(Color::Cyan)),
        );
    f.render_widget(Clear, modal_area);
    f.render_widget(modal_block, modal_area);
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
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
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

/// Handle a key event while the settings modal is active.
pub async fn handle_settings_key(
    settings: &mut SettingsModal,
    key: KeyEvent,
    config: &Config,
    keystore: &mut Keystore,
    agent: &Arc<Mutex<Agent>>,
) -> SettingsHandleResult {
    let mut actions = vec![SettingsAction::Redraw];

    match settings.mode {
        SettingsMode::Adding | SettingsMode::Editing => {
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
                            match crate::llm::probe_server(&url, "", None).await {
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
                                        "   ↳ could not probe /v1/models — enter model manually"
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
                        match crate::llm::probe_server(&url, hint, api_key).await {
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
                                    "   ↳ could not probe /v1/models — edit model manually".into(),
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
            KeyCode::Enter => {
                if settings.selected != settings.active_endpoint_idx {
                    actions.extend(
                        switch_to_endpoint(settings, config, agent, settings.selected).await,
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
            KeyCode::Esc => {
                settings.active = false;
                actions.push(SettingsAction::Close);
            }
            _ => {}
        },
    }

    SettingsHandleResult { actions }
}

fn finish_add(
    settings: &mut SettingsModal,
    config: &Config,
    keystore: &mut Keystore,
) -> Vec<SettingsAction> {
    let mut actions = vec![];

    let api_key_opt = if settings.new_key.is_empty() {
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

    match keystore.add_endpoint(
        &settings.new_label,
        &settings.new_url,
        &settings.new_model,
        api_key_opt,
    ) {
        Ok(()) => actions.push(SettingsAction::Notify(format!(
            "Added endpoint: {}",
            settings.new_label
        ))),
        Err(e) => actions.push(SettingsAction::Notify(format!("Failed to save endpoint: {}", e))),
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
                    actions.extend(switch_to_endpoint(settings, config, agent, 0).await);
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
                let session_endpoint =
                    SettingsModal::session_list_head(keystore, config);
                settings.rebuild_endpoints(&session_endpoint, keystore);
                if list_idx == settings.active_endpoint_idx {
                    actions.extend(switch_to_endpoint(settings, config, agent, list_idx).await);
                }
            }
            Err(e) => actions.push(SettingsAction::Notify(format!("Failed to update endpoint: {}", e))),
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
    idx: usize,
) -> Vec<SettingsAction> {
    let mut actions = vec![];
    let ep = &settings.endpoints[idx];

    actions.push(SettingsAction::Trace(format!(
        "⟳ Switching to: {} ({})",
        ep.label, ep.base_url
    )));

    let mut active = ep.clone();
    let budget = match crate::llm::probe_server(&ep.base_url, &ep.model, ep.api_key.as_deref()).await
    {
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

    if let Ok(mut ag) = agent.try_lock() {
        ag.switch_endpoint(&active, budget.clone());
    }

    actions.push(SettingsAction::DisplayUpdate {
        model: active.model.clone(),
        budget: budget.clone(),
    });
    actions.push(SettingsAction::ActiveIdx(idx));
    actions.push(SettingsAction::Notify(format!(
        "Switched to: {} ({})",
        ep.label, active.model
    )));
    actions
}