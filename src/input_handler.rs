//! Input handling logic for key events and editing actions.

use crate::app_state::App;
use crate::key_edit::{is_paste_key, map_key_to_edit};
use crate::settings_modal::handle_settings_key;
use anyhow::Result;
use crossterm::event::{Event, KeyCode, KeyModifiers};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::sync::Mutex;

use crate::desktop::ActiveDesktop;
use crate::input_dispatch::apply_settings_actions;
use crate::keystore::Keystore;
use crate::tui_render::Pane as RenderPane;
use raven_tui::agent::Agent;
use raven_tui::config::Config;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::sync::Mutex as TokioMutex;

/// Handle a key event and update the app state accordingly.
#[allow(clippy::too_many_arguments)]
pub async fn handle_key_event(
    app: &mut App,
    event: Event,
    config: &Config,
    keystore: &mut Keystore,
    agent: &Arc<TokioMutex<Agent>>,
    balance_tx: &mpsc::Sender<String>,
    queued_interject: &Arc<Mutex<Option<String>>>,
    instant_interject: &Arc<Mutex<Option<String>>>,
    stop: &Arc<AtomicBool>,
    update_tx: mpsc::Sender<crate::event_loop::UiUpdate>,
    approval_req_tx: mpsc::Sender<(String, oneshot::Sender<bool>)>,
) -> Result<bool> {
    let mut handled = false;

    match event {
        Event::Key(key) => {
            handled = handle_key(
                app,
                key,
                config,
                keystore,
                agent,
                balance_tx,
                queued_interject,
                instant_interject,
                stop,
                update_tx.clone(),
                approval_req_tx.clone(),
            )
            .await?;
        }
        Event::Paste(data) => {
            handle_paste(app, &data);
            handled = true;
        }
        Event::Resize(_width, _height) => {
            app.needs_redraw = true;
        }
        _ => {}
    }

    Ok(handled)
}

/// Handle a single key event.
#[allow(clippy::too_many_arguments)]
pub async fn handle_key(
    app: &mut App,
    key: crossterm::event::KeyEvent,
    config: &Config,
    keystore: &mut Keystore,
    agent: &Arc<TokioMutex<Agent>>,
    balance_tx: &mpsc::Sender<String>,
    queued_interject: &Arc<Mutex<Option<String>>>,
    instant_interject: &Arc<Mutex<Option<String>>>,
    stop: &Arc<AtomicBool>,
    update_tx: mpsc::Sender<crate::event_loop::UiUpdate>,
    approval_req_tx: mpsc::Sender<(String, oneshot::Sender<bool>)>,
) -> Result<bool> {
    // Handle settings key first
    if app.settings.active {
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
                app.balance_label = if raven_tui::llm::is_metered_endpoint(&ag.current_config().base_url) {
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
        return Ok(true);
    }

    // Handle mode menu keys (delegates to full impl in App for applying the choice)
    if app.mode_menu_active {
        let consumed = app.handle_mode_menu_key(key, agent).await;
        return Ok(consumed);
    }

    // Handle agent mode menu keys (delegates to full impl in App)
    if app.agent_mode_menu_active {
        let consumed = app.handle_agent_mode_menu_key(key, agent).await;
        return Ok(consumed);
    }

    // Handle search mode
    if app.search_mode {
        return handle_search_key(app, key).await;
    }

    // Handle input keys
    handle_input_key(
        app,
        key,
        config,
        keystore,
        agent,
        queued_interject,
        instant_interject,
        stop,
        update_tx,
        approval_req_tx,
    )
    .await
}



/// Handle search mode keys.
async fn handle_search_key(app: &mut App, key: crossterm::event::KeyEvent) -> Result<bool> {
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            app.search_mode = false;
            app.needs_redraw = true;
            Ok(true)
        }
        KeyCode::Enter => {
            // Perform search
            app.search_mode = false;
            app.needs_redraw = true;
            Ok(true)
        }
        KeyCode::Backspace => {
            app.search.query = app.search.query.trim_end().to_string();
            app.needs_redraw = true;
            Ok(true)
        }
        KeyCode::Char(c) => {
            app.search.query.push(c);
            app.needs_redraw = true;
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// Handle input keys (when not in special modes).
#[allow(clippy::too_many_arguments)]
async fn handle_input_key(
    app: &mut App,
    key: crossterm::event::KeyEvent,
    config: &Config,
    keystore: &mut Keystore,
    agent: &Arc<TokioMutex<Agent>>,
    queued_interject: &Arc<Mutex<Option<String>>>,
    instant_interject: &Arc<Mutex<Option<String>>>,
    stop: &Arc<AtomicBool>,
    update_tx: mpsc::Sender<crate::event_loop::UiUpdate>,
    approval_req_tx: mpsc::Sender<(String, oneshot::Sender<bool>)>,
) -> Result<bool> {
    let modifiers = key.modifiers;
    let is_shift = modifiers.contains(KeyModifiers::SHIFT);
    let is_ctrl = modifiers.contains(KeyModifiers::CONTROL);
    let _is_alt = modifiers.contains(KeyModifiers::ALT);

    // Check for paste key (Shift+Insert or Ctrl+V) -- rely on Event::Paste primarily
    if is_paste_key(&key) {
        // Clipboard read can be handled via Event::Paste; skip here to avoid missing fn
        return Ok(true);
    }

    // Session picker screen navigation (two drop-down lists)
    if app.desktop.showing_picker() && app.handle_picker_key(key.code, agent) {
        return Ok(true);
    }

    // When focused on content panes (Left/Right), use arrows for focus or desktop slide
    if app.focused_pane != crate::app_state::Pane::Input {
        match key.code {
            KeyCode::Left => {
                if app.desktop.showing_picker() {
                    // handled above in picker block
                    return Ok(true);
                }
                if app.desktop.active == ActiveDesktop::Workspace {
                    if app.focused_pane == crate::app_state::Pane::Right {
                        // on Trace, left shifts focus to Conversation pane (stay on 3rd screen)
                        app.focused_pane = crate::app_state::Pane::Left;
                        app.needs_redraw = true;
                        return Ok(true);
                    } else if app.focused_pane == crate::app_state::Pane::Left {
                        // on Conversation, left goes to 2nd screen (picker) focused on session pane
                        app.desktop.set_picker();
                        app.picker.focus = crate::app_state::PickerFocus::Sessions;
                        if !app.picker.loaded {
                            app.refresh_picker();
                        }
                        app.needs_redraw = true;
                        return Ok(true);
                    }
                }
                // Try slide first (for splash/workspace), fallback to focus change
                if !app.try_slide_to_splash() {
                    app.focused_pane = crate::app_state::Pane::Left;
                }
                app.needs_redraw = true;
                return Ok(true);
            }
            KeyCode::Right => {
                if app.desktop.showing_picker() {
                    // handled above
                    return Ok(true);
                }
                // Only enter picker from splash, not from workspace (to prevent right arrow on 3rd screen going back to picker)
                if app.desktop.active == ActiveDesktop::Splash && app.desktop.can_enter_picker() {
                    app.enter_picker();
                    return Ok(true);
                }
                if !app.try_slide_to_workspace() {
                    app.focused_pane = crate::app_state::Pane::Right;
                }
                app.needs_redraw = true;
                return Ok(true);
            }
            _ => {}
        }
    }

    // Slash menu navigation with arrows (or vim keys) when the / command menu is active.
    // This takes precedence over other input when typing a slash command.
    if app.input.starts_with('/') {
        let filtered = get_filtered_commands(&app.slash_commands, &app.input);
        if !filtered.is_empty() {
            match key.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    if app.slash_selected > 0 {
                        app.slash_selected -= 1;
                    }
                    app.needs_redraw = true;
                    return Ok(true);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if app.slash_selected + 1 < filtered.len() {
                        app.slash_selected += 1;
                    }
                    app.needs_redraw = true;
                    return Ok(true);
                }
                _ => {}
            }
        }
    }

    // Suppress text input editing on splash and picker screens (input bar is hidden there)
    if matches!(app.desktop.active, ActiveDesktop::Splash | ActiveDesktop::Picker)
        && matches!(key.code, KeyCode::Char(_) | KeyCode::Backspace | KeyCode::Delete)
    {
        return Ok(true);
    }

    // Map key to edit action (updated API) — only when focused on input
    if app.focused_pane == crate::app_state::Pane::Input {
        if let Some(action) = map_key_to_edit(&key) {
            app.apply_edit_action(action);
            clamp_slash_selection(&app.slash_commands, &app.input, &mut app.slash_selected);
            return Ok(true);
        }
    }

    // Handle special keys
    match key.code {
        KeyCode::Char(c) => {
            app.insert_char(c);
            clamp_slash_selection(&app.slash_commands, &app.input, &mut app.slash_selected);
            Ok(true)
        }
        KeyCode::Enter => {
            if app.is_processing {
                // Submit queued interject or instant interject
                if is_ctrl {
                    app.submit_instant_interject(
                        app.input.clone(),
                        queued_interject,
                        instant_interject,
                        stop,
                    );
                } else {
                    app.submit_queued_interject(app.input.clone(), queued_interject);
                }
            } else {
                // Submit the input. If it is a slash command, dispatch it (may activate
                // submenus for /approval-mode or /run-mode, or handle instantly).
                // Only plain prompts go through to the agent driver.
                let prompt = app.input.trim().to_string();
                if prompt.is_empty() {
                    return Ok(true);
                }
                app.left_committed.push(format!("> {}", prompt));
                app.clear_input();
                app.needs_redraw = true;

                // Prepare slash context (dispatch uses the prompt string; many /cmds mutate via ctx)
                let focused = match app.focused_pane {
                    crate::app_state::Pane::Left => RenderPane::Left,
                    crate::app_state::Pane::Right => RenderPane::Right,
                    crate::app_state::Pane::Input => RenderPane::Right,
                };
                let mut ctx = crate::input_dispatch::SlashContext {
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
                    agent_mode_menu_active: &mut app.agent_mode_menu_active,
                    selected_agent_mode_idx: &mut app.selected_agent_mode_idx,
                    settings: &mut app.settings,
                    search: &mut app.search,
                    focused_pane: focused,
                    left_scroll: &mut app.left_scroll,
                    right_scroll: &mut app.right_scroll,
                    left_follow_output: &mut app.left_follow_output,
                    right_follow_output: &mut app.right_follow_output,
                    last_left_line_count: app.last_left_line_count,
                    last_right_line_count: app.last_right_line_count,
                    last_left_area_h: app.last_left_area.height,
                    last_right_area_h: app.last_right_area.height,
                    config,
                    keystore,
                    agent,
                };

                match crate::input_dispatch::dispatch_slash_command(&prompt, &mut ctx) {
                    crate::input_dispatch::SlashDispatch::AgentPrompt(()) => {
                        app.is_processing = true;
                        app.needs_redraw = true;

                        let agent_c = agent.clone();
                        let tx_c = update_tx.clone();
                        let appr_c = approval_req_tx.clone();
                        let stop_c = stop.clone();
                        let q_c = queued_interject.clone();
                        let i_c = instant_interject.clone();
                        let prompt_c = prompt;

                        tokio::spawn(async move {
                            let mut ag = agent_c.lock().await;
                            let mode = ag.current_exec_mode();
                            let agent_mode = ag.current_agent_mode();
                            let mut obs = crate::event_loop::TuiObserver {
                                tx: tx_c.clone(),
                                approval_req_tx: appr_c,
                                stop: stop_c,
                                queued: q_c,
                                instant: i_c,
                                denials_this_turn: 0,
                                halt_tools: false,
                                exec_mode: mode,
                            };
                            let res = raven_tui::agent_driver::drive_turn(&mut ag, &prompt_c, &mut obs).await;
                            match res {
                                Ok(r) => {
                                    // In "work" mode, run the Super Judge before declaring Done
                                    if agent_mode == "work" {
                                        const MAX_SUPER_JUDGE_CYCLES: u32 = 3;
                                        let mut last_text = r.final_text.clone();
                                        let mut all_actions = r.actions.clone();
                                        let mut cycle = 0u32;

                                        loop {
                                            if cycle >= MAX_SUPER_JUDGE_CYCLES {
                                                let _ = tx_c.send(crate::event_loop::UiUpdate::ToolResult {
                                                    name: "system".into(),
                                                    summary: "🔍 Super Judge: max review cycles reached, accepting".into(),
                                                }).await;
                                                break;
                                            }
                                            cycle += 1;

                                            // Brief pause for UX (shows Processing off briefly)
                                            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                                            let _ = tx_c.send(crate::event_loop::UiUpdate::SuperJudgeBegin).await;
                                            let _ = tx_c.send(crate::event_loop::UiUpdate::ToolResult {
                                                name: "system".into(),
                                                summary: format!("🔍 Super Judge reviewing work (cycle {}/{})", cycle, MAX_SUPER_JUDGE_CYCLES),
                                            }).await;

                                            // Use the headless Super Judge observer
                                            let mut sj_obs = raven_tui::super_judge::SuperJudgeObserver::new();
                                            let verdict = raven_tui::super_judge::run_super_judge_with_observer(
                                                &mut ag, &last_text, &all_actions, &mut sj_obs,
                                            ).await;

                                            match verdict {
                                                raven_tui::super_judge::SuperJudgeVerdict::Complete { note } => {
                                                    let _ = tx_c.send(crate::event_loop::UiUpdate::ToolResult {
                                                        name: "system".into(),
                                                        summary: format!("🔍 Super Judge: WORK_COMPLETE — {}", note),
                                                    }).await;
                                                    break;
                                                }
                                                raven_tui::super_judge::SuperJudgeVerdict::Continue { feedback } => {
                                                    let _ = tx_c.send(crate::event_loop::UiUpdate::ToolResult {
                                                        name: "system".into(),
                                                        summary: format!("🔍 Super Judge: NEEDS_WORK — nudging agent"),
                                                    }).await;
                                                    // Inject feedback and re-run
                                                    let nudge = format!(
                                                        "[🔍 SUPER JUDGE FEEDBACK]: {}\n\nContinue working on the task.",
                                                        feedback
                                                    );
                                                    match raven_tui::agent_driver::drive_turn(&mut ag, &nudge, &mut obs).await {
                                                        Ok(r2) => {
                                                            last_text = r2.final_text.clone();
                                                            all_actions.extend(r2.actions);
                                                        }
                                                        Err(e) => {
                                                            let _ = tx_c.send(crate::event_loop::UiUpdate::Error(
                                                                format!("Super Judge re-run error: {}", e)
                                                            )).await;
                                                            break;
                                                        }
                                                    }
                                                }
                                                raven_tui::super_judge::SuperJudgeVerdict::DeathSpiral { feedback } => {
                                                    let _ = tx_c.send(crate::event_loop::UiUpdate::ToolResult {
                                                        name: "system".into(),
                                                        summary: format!("🔍 Super Judge: DEATH_SPIRAL detected — {}", feedback),
                                                    }).await;
                                                    // Inject anti-spiral guidance as final message
                                                    ag.push_message("user", &format!(
                                                        "[🔍 SUPER JUDGE — DEATH SPIRAL DETECTED]: {}\n\n\
                                                         Stop repeating the same approach. Try something completely different.",
                                                        feedback
                                                    ));
                                                    break;
                                                }
                                                raven_tui::super_judge::SuperJudgeVerdict::Skipped { reason } => {
                                                    let _ = tx_c.send(crate::event_loop::UiUpdate::ToolResult {
                                                        name: "system".into(),
                                                        summary: format!("🔍 Super Judge: skipped — {}", reason),
                                                    }).await;
                                                    break;
                                                }
                                            }
                                        }

                                        let _ = tx_c.send(crate::event_loop::UiUpdate::Done {
                                            final_text: last_text,
                                        }).await;
                                    } else {
                                        let _ = tx_c.send(crate::event_loop::UiUpdate::Done {
                                            final_text: r.final_text,
                                        }).await;
                                    }
                                    let _ = tx_c.send(crate::event_loop::UiUpdate::Usage {
                                        prompt_tokens: Some(r.metrics.prompt_tokens as u32),
                                        completion_tokens: Some(r.metrics.completion_tokens as u32),
                                        total_tokens: Some(r.metrics.total_tokens as u32),
                                    }).await;
                                }
                                Err(e) => {
                                    let _ = tx_c.send(crate::event_loop::UiUpdate::Error(e.to_string())).await;
                                }
                            }
                        });
                    }
                    crate::input_dispatch::SlashDispatch::Handled => {
                        app.needs_redraw = true;
                    }
                    crate::input_dispatch::SlashDispatch::Quit => {
                        stop.store(true, std::sync::atomic::Ordering::SeqCst);
                    }
                }
            }
            Ok(true)
        }
        KeyCode::Tab => {
            if is_shift {
                app.cycle_focus_backward();
            } else {
                app.cycle_focus_forward();
            }
            Ok(true)
        }
        KeyCode::Esc => {
            // Clear input or close menus
            if app.search_mode {
                app.search_mode = false;
            } else if app.mode_menu_active {
                app.mode_menu_active = false;
            } else if app.agent_mode_menu_active {
                app.agent_mode_menu_active = false;
            } else if !app.input.is_empty() {
                app.clear_input();
            }
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// Handle paste data.
pub fn handle_paste(app: &mut App, data: &str) {
    let sanitized: String = data
        .chars()
        .filter(|c| *c == '\n' || *c == '\t' || !c.is_control())
        .collect();
    if sanitized.is_empty() {
        return;
    }
    app.insert_str_at_cursor(&sanitized);
    app.history_index = None;
    clamp_slash_selection(&app.slash_commands, &app.input, &mut app.slash_selected);
    app.needs_redraw = true;
}

/// Schedule a balance refresh.
pub fn schedule_balance_refresh(agent: &Arc<TokioMutex<Agent>>, tx: &mpsc::Sender<String>) {
    let agent2 = agent.clone();
    let btx = tx.clone();
    tokio::spawn(async move {
        refresh_balance_label(&agent2, &btx).await;
    });
}

/// Refresh the balance label.
pub async fn refresh_balance_label(agent: &Arc<TokioMutex<Agent>>, tx: &mpsc::Sender<String>) {
    let (base_url, api_key) = {
        let ag = agent.lock().await;
        let cfg = ag.current_config();
        (cfg.base_url.clone(), cfg.api_key.clone())
    };
    let label = raven_tui::llm::balance_label_for(&base_url, api_key.as_deref()).await;
    let _ = tx.send(label).await;
}

/// Clamp slash menu selection.
pub fn clamp_slash_selection(commands: &[crate::input_dispatch::SlashCommand], input: &str, selected: &mut usize) {
    let filtered = get_filtered_commands(commands, input);
    if !filtered.is_empty() {
        *selected = (*selected).min(filtered.len().saturating_sub(1));
    } else {
        *selected = 0;
    }
}

/// Get filtered slash commands based on input.
pub fn get_filtered_commands<'a>(
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


