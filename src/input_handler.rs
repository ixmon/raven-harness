//! Input handling logic for key events and editing actions.

use crate::app_state::App;
use crate::key_edit::{is_paste_key, map_key_to_edit, EditAction};
use crate::settings_modal::handle_settings_key;
use anyhow::Result;
use crossterm::event::{Event, KeyCode, KeyModifiers};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::sync::Mutex;

use crate::desktop::ActiveDesktop;
use crate::input_dispatch::apply_settings_actions;
use crate::keystore::Keystore;
use raven_tui::agent::Agent;
use raven_tui::config::Config;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::sync::Mutex as TokioMutex;

use crate::plan_flow::{
    dispatch_plan_slash, start_plan_execution,
    plan_loop_active, route_plan_entry_intent, spawn_plan_answer_submit,
    spawn_proceed_feedback_work, submit_plan_loop_input,
    PlanInputRouting, PlanLoopUserOutcome,
};

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
            if app.settings.active && matches!(
                app.settings.mode,
                crate::settings_modal::SettingsMode::Adding
                | crate::settings_modal::SettingsMode::Editing
                | crate::settings_modal::SettingsMode::BraveKey
            ) {
                handle_settings_paste(&mut app.settings, &data);
            } else {
                handle_paste(app, &data);
            }
            handled = true;
        }
        Event::Resize(_width, _height) => {
            app.needs_redraw = true;
        }
        Event::Mouse(me) => {
            if let crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left) = me.kind {
                let col = me.column;
                let row = me.row;
                if app.desktop.showing_picker() {
                    app.picker.focus = crate::app_state::PickerFocus::Tree;
                    app.needs_redraw = true;
                    handled = true;
                } else if matches!(app.desktop.active, ActiveDesktop::Workspace) {
                    // Use last rendered areas to decide which pane was clicked
                    let in_left = app.last_left_area.x <= col && col < app.last_left_area.x + app.last_left_area.width
                        && app.last_left_area.y <= row && row < app.last_left_area.y + app.last_left_area.height;
                    let in_right = app.last_right_area.x <= col && col < app.last_right_area.x + app.last_right_area.width
                        && app.last_right_area.y <= row && row < app.last_right_area.y + app.last_right_area.height;
                    if in_left {
                        app.focused_pane = crate::app_state::Pane::Left;
                        app.needs_redraw = true;
                        handled = true;
                    } else if in_right {
                        app.focused_pane = crate::app_state::Pane::Right;
                        app.needs_redraw = true;
                        handled = true;
                    } else {
                        app.focused_pane = crate::app_state::Pane::Input;
                        app.needs_redraw = true;
                        handled = true;
                    }
                }
            }
            // other mouse events (wheel, up, drag) ignored for now
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
            agent,
            keystore,
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

/// Dispatch a slash command from the input buffer (shared by idle and in-flight paths).
#[allow(clippy::too_many_arguments)]
async fn dispatch_slash_input(
    app: &mut App,
    config: &Config,
    keystore: &Keystore,
    agent: &Arc<TokioMutex<Agent>>,
    prompt: &str,
    stop: &Arc<AtomicBool>,
    update_tx: &mpsc::Sender<crate::event_loop::UiUpdate>,
    approval_req_tx: mpsc::Sender<(String, oneshot::Sender<bool>)>,
    queued_interject: &Arc<Mutex<Option<String>>>,
    instant_interject: &Arc<Mutex<Option<String>>>,
    spawn_agent_on_prompt: bool,
) -> crate::input_dispatch::SlashDispatch {
    use crate::tui_render::Pane as RenderPane;

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

    let result = crate::input_dispatch::dispatch_slash_command(prompt, &mut ctx);
    if spawn_agent_on_prompt {
        if let crate::input_dispatch::SlashDispatch::AgentPrompt(()) = result {
            spawn_agent_turn(
                app,
                agent,
                prompt.to_string(),
                stop,
                update_tx,
                &approval_req_tx,
                queued_interject,
                instant_interject,
            );
        }
    }
    result
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

    // Session/workspace picker screen navigation (combined tree of workspaces + sessions)
    // For overview + harness (Coding Harness nav), we let input/conv have priority on arrows so status/conv/input work.
    let overview_harness = app.desktop.active == ActiveDesktop::Overview
        && app.browser_selected_is_harness();
    if (app.desktop.showing_picker()
        || app.desktop.active == ActiveDesktop::Splash
        || app.desktop.active == ActiveDesktop::Overview)
        && !(overview_harness && app.focused_pane == crate::app_state::Pane::Input)
        && app.handle_picker_key(key.code, agent)
    {
        return Ok(true);
    }

    // Full wiki viewer screen
    if app.desktop.showing_wiki_viewer() && app.handle_wiki_viewer_key(key.code, agent) {
        return Ok(true);
    }

    // When focused on content panes (Left/Right), use arrows for focus or desktop slide
    // (skip when wiki viewer owns the screen — its handler already processed or rejected the key)
    // Also skip for overview+harness so arrows don't accidentally slide while using conv/input
    if app.focused_pane != crate::app_state::Pane::Input && !app.desktop.showing_wiki_viewer() {
        match key.code {
            KeyCode::Left => {
                if app.desktop.showing_picker() {
                    // handled above in picker block
                    return Ok(true);
                }
                if app.desktop.active == ActiveDesktop::Overview {
                    // handled by handle_picker_key (cycles focus or exits to splash)
                    return Ok(true);
                }
                if app.desktop.active == ActiveDesktop::Workspace {
                    if app.focused_pane == crate::app_state::Pane::Right {
                        // on Trace, left shifts focus to Conversation pane (stay on 3rd screen)
                        app.focused_pane = crate::app_state::Pane::Left;
                        app.needs_redraw = true;
                        return Ok(true);
                    } else if app.focused_pane == crate::app_state::Pane::Left {
                        // on Conversation, left from Screen 4 goes back to Screen 2 (with content focused)
                        app.desktop.set_overview();
                        app.view_focus = crate::app_state::ViewFocus::Content;
                        if !app.picker.loaded {
                            app.refresh_picker();
                        }
                        if app.browser_nav_items.is_empty() {
                            let sid = app.picker.sessions.get(app.picker.selected_session).map(|m| m.session_id.clone());
                            if let Some(sid) = sid {
                                app.rebuild_browser_nav_for_session(&sid);
                            }
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
                if app.desktop.active == ActiveDesktop::Overview {
                    // focus cycle / snap handled in handle_picker_key
                    return Ok(true);
                }
                // Note: splash focus cycling and overview slide are handled earlier via handle_picker_key for Splash/Overview.
                if !app.try_slide_to_workspace() {
                    app.focused_pane = crate::app_state::Pane::Right;
                }
                app.needs_redraw = true;
                return Ok(true);
            }
            KeyCode::Up | KeyCode::Char('k') => {
                app.scroll_focused_line(-1);
                app.needs_redraw = true;
                return Ok(true);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                app.scroll_focused_line(1);
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

    // Suppress text input editing on splash/picker/overview screens (input bar is hidden there)
    // except when in adding workspace mode
    let overview_harness = matches!(app.desktop.active, ActiveDesktop::Overview)
        && app.browser_selected_is_harness();
    if matches!(app.desktop.active, ActiveDesktop::Splash | ActiveDesktop::Picker | ActiveDesktop::Overview)
        && !overview_harness
        && !app.picker.adding_workspace
        && matches!(key.code, KeyCode::Char(_) | KeyCode::Backspace | KeyCode::Delete)
    {
        return Ok(true);
    }

    // Map key to edit action (updated API).
    // Editing the input (backspace, delete, etc.) is allowed whenever the input
    // is visible (on workspace). Cursor movement in input only when explicitly
    // focused on the input pane (otherwise arrows navigate panes).
    if let Some(action) = map_key_to_edit(&key) {
        let is_cursor_move = matches!(
            action,
            EditAction::Left | EditAction::Right | EditAction::Home | EditAction::End
        );
        if !is_cursor_move || app.focused_pane == crate::app_state::Pane::Input {
            app.apply_edit_action(action);
            clamp_slash_selection(&app.slash_commands, &app.input, &mut app.slash_selected);
            if !matches!(app.desktop.active, ActiveDesktop::Splash | ActiveDesktop::Picker | ActiveDesktop::Overview) || overview_harness || app.picker.adding_workspace {
                app.focused_pane = crate::app_state::Pane::Input;
            }
            return Ok(true);
        }
    }

    // Handle special keys
    match key.code {
        KeyCode::Char(c) => {
            app.insert_char(c);
            clamp_slash_selection(&app.slash_commands, &app.input, &mut app.slash_selected);
            if !matches!(app.desktop.active, ActiveDesktop::Splash | ActiveDesktop::Picker | ActiveDesktop::Overview) || overview_harness || app.picker.adding_workspace {
                app.focused_pane = crate::app_state::Pane::Input;
            }
            Ok(true)
        }
        KeyCode::Enter => {
            if app.is_processing {
                let prompt = app.input.trim().to_string();
                if !prompt.is_empty()
                    && crate::input_dispatch::slash_ok_while_processing(&prompt)
                {
                    match dispatch_slash_input(
                        app,
                        config,
                        keystore,
                        agent,
                        &prompt,
                        stop,
                        &update_tx,
                        approval_req_tx.clone(),
                        queued_interject,
                        instant_interject,
                        false,
                    )
                    .await
                    {
                        crate::input_dispatch::SlashDispatch::Handled => {
                            app.needs_redraw = true;
                            return Ok(true);
                        }
                        crate::input_dispatch::SlashDispatch::Quit => {
                            stop.store(true, std::sync::atomic::Ordering::SeqCst);
                            return Ok(true);
                        }
                        crate::input_dispatch::SlashDispatch::AgentPrompt(()) => {}
                    }
                }
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
                // Picker add workspace: Enter accepts the path and starts trust confirm
                if app.desktop.showing_picker() && app.picker.adding_workspace {
                    let path = app.input.trim().to_string();
                    app.clear_input();
                    app.picker.adding_workspace = false;
                    app.needs_redraw = true;
                    if !path.is_empty() {
                        if let Ok(p) = std::fs::canonicalize(&path) {
                            if p.is_dir() {
                                app.picker.confirm_trust_path = Some(p.clone());
                                app.input = format!("Trust {} ? [y/n]", p.display());
                                app.cursor_pos = app.input.len();
                            }
                        }
                    }
                    app.needs_redraw = true;
                    return Ok(true);
                }
                // Submit the input. If it is a slash command, dispatch it (may activate
                // submenus for /approval-mode or /run-mode, or handle instantly).
                // Only plain prompts go through to the agent driver.
                let prompt = app.input.trim().to_string();
                if prompt.is_empty() {
                    return Ok(true);
                }

                let backend = {
                    let ag = agent.lock().await;
                    ag.llm_backend()
                };
                let flags = &config.flags;
                let workspace = config.workspace.display().to_string();

                if prompt.starts_with("/plan") {
                    app.clear_input();
                    app.needs_redraw = true;
                    let _ = dispatch_plan_slash(&prompt, app, agent);
                    return Ok(true);
                }

                if app.plan.pending_observe_prompt.is_some() {
                    if let Ok(mut ag) = agent.try_lock() {
                        if let Some(msg) = ag.apply_user_observation(&prompt) {
                            crate::plan_sync::sync_plan_from_agent(&mut app.plan, &ag);
                            app.left_committed.push(format!("> {}", prompt));
                            app.left_committed.push(msg.clone());
                            ag.push_message(
                                "user",
                                &format!(
                                    "[User observation recorded] {}\n\nContinue with the plan from the next step.",
                                    prompt
                                ),
                            );
                            app.clear_input();
                            app.needs_redraw = true;
                            // Fall through to dispatch as agent prompt to resume work.
                        }
                    }
                }

                let mut agent_prompt = prompt.clone();
                if plan_loop_active(&app.plan) {
                    app.clear_input();
                    match submit_plan_loop_input(app, agent, &prompt) {
                        PlanLoopUserOutcome::SpawnAnswer {
                            user_input,
                            question,
                        } => {
                            app.is_processing = true;
                            app.needs_redraw = true;
                            spawn_plan_answer_submit(
                                app,
                                backend.clone(),
                                flags.clone(),
                                workspace.clone(),
                                update_tx.clone(),
                                user_input,
                                question,
                            );
                            return Ok(true);
                        }
                        PlanLoopUserOutcome::SpawnProceedFeedback { user_input } => {
                            app.is_processing = true;
                            app.needs_redraw = true;
                            spawn_proceed_feedback_work(
                                app,
                                backend.clone(),
                                flags.clone(),
                                workspace.clone(),
                                update_tx.clone(),
                                user_input,
                            );
                            return Ok(true);
                        }
                        PlanLoopUserOutcome::StartExecution => {
                            agent_prompt =
                                start_plan_execution(app, agent, &config.workspace);
                        }
                        PlanLoopUserOutcome::Consumed => {
                            return Ok(true);
                        }
                    }
                }

                match route_plan_entry_intent(app, agent, &backend, flags, &prompt).await {
                    PlanInputRouting::Stop => {
                        app.needs_redraw = true;
                        return Ok(true);
                    }
                    PlanInputRouting::Continue | PlanInputRouting::Pass => {}
                }

                if !agent_prompt.starts_with("Execute the approved plan.") {
                    app.left_committed.push(format!("> {}", agent_prompt));
                }
                app.clear_input();
                app.needs_redraw = true;

                match dispatch_slash_input(
                    app,
                    config,
                    keystore,
                    agent,
                    &agent_prompt,
                    stop,
                    &update_tx,
                    approval_req_tx.clone(),
                    queued_interject,
                    instant_interject,
                    true,
                )
                .await
                {
                    crate::input_dispatch::SlashDispatch::Handled => {
                        app.needs_redraw = true;
                    }
                    crate::input_dispatch::SlashDispatch::Quit => {
                        stop.store(true, std::sync::atomic::Ordering::SeqCst);
                    }
                    crate::input_dispatch::SlashDispatch::AgentPrompt(()) => {}
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
            if app.is_processing {
                // Signal the running drive_turn (via TuiObserver) to stop at the next
                // safe point. We intentionally do NOT clear is_processing here: the
                // post-input "stop && !is_processing => quit app" check would otherwise
                // exit the TUI. The turn will send Done (which clears is_processing and
                // resets the stop flag). Gauge drawing suppresses while stop is asserted.
                // Stop takes effect at round boundaries (after current tool batch or
                // sooner for remaining tools in the batch).
                stop.store(true, std::sync::atomic::Ordering::SeqCst);
                app.trace_lines.push("⏹ Stopped by user (Esc)".to_string());
                app.right_follow_output = true;
                app.right_scroll = 10_000;
                app.needs_redraw = true;
                return Ok(true);
            }
            // Clear input or close menus
            if app.search_mode {
                app.search_mode = false;
            } else if app.mode_menu_active {
                app.mode_menu_active = false;
            } else if app.agent_mode_menu_active {
                app.agent_mode_menu_active = false;
            } else if app.desktop.showing_picker() && app.picker.adding_workspace {
                app.picker.adding_workspace = false;
                app.clear_input();
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

/// Handle paste into settings modal edit buffer.
fn handle_settings_paste(settings: &mut crate::settings_modal::SettingsModal, data: &str) {
    let sanitized: String = data
        .chars()
        .filter(|c| !c.is_control())
        .collect();
    if !sanitized.is_empty() {
        settings.apply_edit_action(EditAction::InsertStr(sanitized));
    }
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

/// Run an agent turn in the background so the TUI event loop can keep animating.
#[allow(clippy::too_many_arguments)]
pub fn spawn_agent_turn(
    app: &mut App,
    agent: &Arc<TokioMutex<Agent>>,
    prompt: String,
    stop: &Arc<AtomicBool>,
    update_tx: &mpsc::Sender<crate::event_loop::UiUpdate>,
    approval_req_tx: &mpsc::Sender<(String, oneshot::Sender<bool>)>,
    queued_interject: &Arc<Mutex<Option<String>>>,
    instant_interject: &Arc<Mutex<Option<String>>>,
) {
    stop.store(false, std::sync::atomic::Ordering::SeqCst);
    app.is_processing = true;
    app.needs_redraw = true;

    let agent_c = agent.clone();
    let tx_c = update_tx.clone();
    let appr_c = approval_req_tx.clone();
    let stop_c = stop.clone();
    let q_c = queued_interject.clone();
    let i_c = instant_interject.clone();
    let prompt_c = prompt;
    let exec_mode_c = app.live_exec_mode.clone();

    if let Ok(mut ag) = agent.try_lock() {
        if let Ok(mut slot) = app.live_exec_mode.lock() {
            *slot = ag.current_exec_mode();
        }
        let workspace = ag.workspace().to_path_buf();
        let wiki = ag
            .session()
            .as_ref()
            .and_then(|s| s.read_wiki_file_raw("plan.md").ok());
        crate::plan_sync::reconcile_plan_execution(&mut app.plan, &ag, wiki.as_deref());
        crate::plan_sync::sync_plan_to_agent(&mut ag, &app.plan, &workspace);
    }

    tokio::spawn(async move {
        let mut ag = agent_c.lock().await;
        let agent_mode = ag.current_agent_mode();
        let mut obs = crate::event_loop::TuiObserver {
            tx: tx_c.clone(),
            approval_req_tx: appr_c,
            stop: stop_c.clone(),
            queued: q_c,
            instant: i_c,
            denials_this_turn: 0,
            halt_tools: false,
            exec_mode: exec_mode_c,
        };
        let res = raven_tui::agent_driver::drive_turn(&mut ag, &prompt_c, &mut obs).await;
        match res {
            Ok(r) => {
                // Plan execution has per-step verification; Super Judge mid-plan derails
                // the agent (denies patches during review, false DEATH_SPIRAL on normal edits).
                let plan_executing = ag.plan_tool_context().plan_executing;
                if agent_mode == "work" && !plan_executing {
                    const MAX_SUPER_JUDGE_CYCLES: u32 = 3;
                    let mut last_text = r.final_text.clone();
                    let mut all_actions = r.actions.clone();
                    let mut cycle = 0u32;

                    loop {
                        if stop_c.load(std::sync::atomic::Ordering::SeqCst) {
                            let _ = tx_c
                                .send(crate::event_loop::UiUpdate::ToolResult {
                                    name: "system".into(),
                                    summary: "⏹ Stopped (Esc)".into(),
                                })
                                .await;
                            break;
                        }
                        if cycle >= MAX_SUPER_JUDGE_CYCLES {
                            let _ = tx_c
                                .send(crate::event_loop::UiUpdate::ToolResult {
                                    name: "system".into(),
                                    summary:
                                        "🔍 Super Judge: max review cycles reached, accepting"
                                            .into(),
                                })
                                .await;
                            break;
                        }
                        cycle += 1;

                        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                        if stop_c.load(std::sync::atomic::Ordering::SeqCst) {
                            let _ = tx_c
                                .send(crate::event_loop::UiUpdate::ToolResult {
                                    name: "system".into(),
                                    summary: "⏹ Stopped (Esc)".into(),
                                })
                                .await;
                            break;
                        }
                        let _ = tx_c
                            .send(crate::event_loop::UiUpdate::SuperJudgeBegin)
                            .await;
                        let _ = tx_c
                            .send(crate::event_loop::UiUpdate::ToolResult {
                                name: "system".into(),
                                summary: format!(
                                    "🔍 Super Judge reviewing work (cycle {}/{})",
                                    cycle, MAX_SUPER_JUDGE_CYCLES
                                ),
                            })
                            .await;

                        let mut sj_obs = raven_tui::super_judge::SuperJudgeObserver::new();
                        let verdict = raven_tui::super_judge::run_super_judge_with_observer(
                            &mut ag, &last_text, &all_actions, &mut sj_obs,
                        )
                        .await;

                        if stop_c.load(std::sync::atomic::Ordering::SeqCst) {
                            let _ = tx_c
                                .send(crate::event_loop::UiUpdate::ToolResult {
                                    name: "system".into(),
                                    summary: "⏹ Stopped (Esc)".into(),
                                })
                                .await;
                            break;
                        }

                        match verdict {
                            raven_tui::super_judge::SuperJudgeVerdict::Complete { note } => {
                                let _ = tx_c
                                    .send(crate::event_loop::UiUpdate::ToolResult {
                                        name: "system".into(),
                                        summary: format!(
                                            "🔍 Super Judge: WORK_COMPLETE — {}",
                                            note
                                        ),
                                    })
                                    .await;
                                break;
                            }
                            raven_tui::super_judge::SuperJudgeVerdict::Continue { feedback } => {
                                let _ = tx_c
                                    .send(crate::event_loop::UiUpdate::ToolResult {
                                        name: "system".into(),
                                        summary: "🔍 Super Judge: NEEDS_WORK — nudging agent"
                                            .to_string(),
                                    })
                                    .await;
                                if stop_c.load(std::sync::atomic::Ordering::SeqCst) {
                                    let _ = tx_c
                                        .send(crate::event_loop::UiUpdate::ToolResult {
                                            name: "system".into(),
                                            summary: "⏹ Stopped (Esc)".into(),
                                        })
                                        .await;
                                    break;
                                }
                                let nudge = format!(
                                    "[🔍 SUPER JUDGE FEEDBACK]: {}\n\nContinue working on the task.",
                                    feedback
                                );
                                match raven_tui::agent_driver::drive_turn(&mut ag, &nudge, &mut obs)
                                    .await
                                {
                                    Ok(r2) => {
                                        last_text = r2.final_text.clone();
                                        all_actions.extend(r2.actions);
                                    }
                                    Err(e) => {
                                        let _ = tx_c
                                            .send(crate::event_loop::UiUpdate::Error(format!(
                                                "Super Judge re-run error: {}",
                                                e
                                            )))
                                            .await;
                                        break;
                                    }
                                }
                            }
                            raven_tui::super_judge::SuperJudgeVerdict::DeathSpiral { feedback } => {
                                let _ = tx_c
                                    .send(crate::event_loop::UiUpdate::ToolResult {
                                        name: "system".into(),
                                        summary: format!(
                                            "🔍 Super Judge: DEATH_SPIRAL detected — {}",
                                            feedback
                                        ),
                                    })
                                    .await;
                                if stop_c.load(std::sync::atomic::Ordering::SeqCst) {
                                    let _ = tx_c
                                        .send(crate::event_loop::UiUpdate::ToolResult {
                                            name: "system".into(),
                                            summary: "⏹ Stopped (Esc)".into(),
                                        })
                                        .await;
                                    break;
                                }
                                let nudge = format!(
                                    "[🔍 SUPER JUDGE — DEATH SPIRAL DETECTED]: {}\n\n\
                                     Stop repeating the same approach. Try something completely different.",
                                    feedback
                                );
                                match raven_tui::agent_driver::drive_turn(&mut ag, &nudge, &mut obs)
                                    .await
                                {
                                    Ok(r2) => {
                                        last_text = r2.final_text.clone();
                                        all_actions.extend(r2.actions);
                                    }
                                    Err(e) => {
                                        let _ = tx_c
                                            .send(crate::event_loop::UiUpdate::Error(format!(
                                                "Super Judge re-run error: {}",
                                                e
                                            )))
                                            .await;
                                    }
                                }
                                break;
                            }
                            raven_tui::super_judge::SuperJudgeVerdict::Skipped { reason } => {
                                let _ = tx_c
                                    .send(crate::event_loop::UiUpdate::ToolResult {
                                        name: "system".into(),
                                        summary: format!("🔍 Super Judge: skipped — {}", reason),
                                    })
                                    .await;
                                break;
                            }
                        }
                    }

                    let _ = tx_c
                        .send(crate::event_loop::UiUpdate::Done {
                            final_text: last_text,
                        })
                        .await;
                } else {
                    let _ = tx_c
                        .send(crate::event_loop::UiUpdate::Done {
                            final_text: r.final_text,
                        })
                        .await;
                }
                let _ = tx_c
                    .send(crate::event_loop::UiUpdate::Usage {
                        prompt_tokens: Some(r.metrics.prompt_tokens as u32),
                        completion_tokens: Some(r.metrics.completion_tokens as u32),
                        total_tokens: Some(r.metrics.total_tokens as u32),
                    })
                    .await;
            }
            Err(e) => {
                let _ = tx_c
                    .send(crate::event_loop::UiUpdate::Error(e.to_string()))
                    .await;
            }
        }
    });
}




