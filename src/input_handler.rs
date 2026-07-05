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
use crate::tui_render::Pane as RenderPane;
use raven_tui::agent::Agent;
use raven_tui::config::Config;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::sync::Mutex as TokioMutex;

/// Returns true if the prompt looks like a request to enter plan mode.
/// Extracted for testability.
pub(crate) fn is_plan_trigger_phrase(prompt: &str) -> bool {
    let lower = prompt.to_lowercase();
    let trigger_phrases = [
        "come up with a plan", "let's plan", "make a plan", "first plan",
        "what's the plan", "plan the", "create a plan", "develop a plan",
        "plan out", "plan for this"
    ];
    trigger_phrases.iter().any(|p| lower.contains(p)) ||
        (lower.contains("plan") && (lower.contains("task") || lower.contains("work") || lower.contains("refactor") || lower.contains("change") || lower.contains("implement")))
}

/// Returns true for phrases that should confirm "proceed" while in plan mode.
pub(crate) fn is_proceed_confirmation(prompt: &str) -> bool {
    let lower = prompt.to_lowercase();
    lower.contains("proceed") || lower.contains("go ahead") || lower.contains("let's go") || lower == "yes" || lower.contains("start executing") || lower.contains("let's do it") || lower.contains("do it") || lower.contains("go for it") || lower.contains("confirmed")
}

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
    if app.desktop.showing_picker() && app.handle_picker_key(key.code, agent) {
        return Ok(true);
    }

    // Full wiki viewer screen
    if app.desktop.showing_wiki_viewer() && app.handle_wiki_viewer_key(key.code, agent) {
        return Ok(true);
    }

    // When focused on content panes (Left/Right), use arrows for focus or desktop slide
    // (skip when wiki viewer owns the screen — its handler already processed or rejected the key)
    if app.focused_pane != crate::app_state::Pane::Input && !app.desktop.showing_wiki_viewer() {
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
                        // on Conversation, left goes to wiki viewer if possible, else picker
                        if !app.wiki_viewer.session_id.is_empty() {
                            app.desktop.set_wiki_viewer();
                        } else {
                            app.desktop.set_picker();
                            app.picker.focus = crate::app_state::PickerFocus::Tree;
                            if !app.picker.loaded {
                                app.refresh_picker();
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

    // Suppress text input editing on splash and picker screens (input bar is hidden there)
    // except when in adding workspace mode
    if matches!(app.desktop.active, ActiveDesktop::Splash | ActiveDesktop::Picker)
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
            if !matches!(app.desktop.active, ActiveDesktop::Splash | ActiveDesktop::Picker) || app.picker.adding_workspace {
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
            if !matches!(app.desktop.active, ActiveDesktop::Splash | ActiveDesktop::Picker) || app.picker.adding_workspace {
                app.focused_pane = crate::app_state::Pane::Input;
            }
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
                // Plan mode entry confirmation dialog
                if app.pending_plan_confirmation {
                    app.pending_plan_confirmation = false;
                    let submitted = app.input.trim().to_lowercase();
                    let yes = submitted.starts_with('y');
                    let original = app.pending_plan_request.take();
                    app.clear_input();
                    if yes {
                        if let Ok(mut ag) = agent.try_lock() {
                            ag.set_agent_mode("plan");
                            if let Some(s) = &mut ag.session_mut() {
                                let _ = s.save_meta();
                            }
                        }
                        app.plan.active = true;
                        app.focused_pane = crate::app_state::Pane::Input; // leave focus in input for answers
                        // Write initial plan to wiki (with task-appropriate verification)
                        if let Ok(mut ag) = agent.try_lock() {
                            if let Some(s) = ag.session_mut() {
                                let verif = app.plan.verification_steps.iter()
                                    .map(|v| format!("- {}", v))
                                    .collect::<Vec<_>>()
                                    .join("\n");
                                // Always initialize a clean, structured template on entering plan mode.
                                // No need to manually delete wiki/plan.md between plan attempts.
                                let plan_text = format!(
                                    "# Plan\n\n**Goal:** {}\n\n**Success Criteria:** {}\n\n**Verification:**\n{}\n\n**Rollback:** {}\n\n**Constraints:** {}\n\n## Notes\n\n(Agent will refine this during clarification. Final approved version written on 'proceed'.)\n",
                                    app.plan.goal, app.plan.success_criteria, verif, app.plan.rollback, app.plan.constraints
                                );
                                let _ = s.write_wiki_file("plan.md", &plan_text);
                                app.left_committed.push("Plan written to session wiki/plan.md (you can edit externally too)".to_string());
                            }
                        }
                        // Populate from the original request if we have it. Always use the triggering request as the Goal.
                        if let Some(req) = original {
                            app.plan.goal = req.clone();
                            // Also update session meta so the goal is persisted and used by agent/judge
                            if let Ok(mut ag) = agent.try_lock() {
                                if let Some(s) = &mut ag.session_mut() {
                                    s.meta.current_goal = req.clone();
                                    let _ = s.save_meta();
                                }
                            }
                        } else if app.plan.goal.is_empty() {
                            // fallback only if no specific request captured
                            app.plan.goal = "Plan for the current task".to_string();
                        }
                        if app.plan.success_criteria.is_empty() {
                            app.plan.success_criteria = "Verification steps pass and the goal is achieved".to_string();
                        }
                        if app.plan.verification_steps.is_empty() {
                            let g = app.plan.goal.to_lowercase();
                            if g.contains("python") || g.contains(".py") {
                                app.plan.verification_steps = vec![
                                    "python3 <your_script>.py [args]".to_string(),
                                    "check that output matches the expected result".to_string(),
                                ];
                            } else if g.contains("c++") || g.contains("cpp") || g.contains("g++") || g.contains("clang") {
                                app.plan.verification_steps = vec![
                                    "g++ -std=c++17 -Wall -o program program.cpp".to_string(),
                                    "clang-tidy program.cpp -- -std=c++17".to_string(),
                                    "./program".to_string(),
                                ];
                            } else if g.contains("c ") || g.contains("gcc") {
                                app.plan.verification_steps = vec![
                                    "gcc -Wall -o program program.c".to_string(),
                                    "./program".to_string(),
                                ];
                            } else {
                                app.plan.verification_steps = vec!["cargo check".to_string(), "cargo clippy -- -D warnings".to_string(), "cargo test".to_string()];
                            }
                        }

                        if app.plan.rollback.is_empty() {
                            app.plan.rollback = "git branch + checkpoints".to_string();
                        }
                        // Do NOT populate steps yet -- they appear only after user approves the full plan
                        app.plan.steps.clear();
                        app.plan.current_step = 0;
                        app.left_committed.push("Entered Plan Mode. Run Mode set to 'plan'.".to_string());
                        // Re-submit the original request now that we are in plan mode so the agent starts clarification
                        if let Some(req) = app.pending_plan_request.take() {
                            app.input = req;
                        } else if !app.plan.goal.is_empty() {
                            app.input = app.plan.goal.clone();
                        }
                        // fall through to normal submit with the (now set) input
                    } else {
                        app.left_committed.push("Plan mode entry cancelled.".to_string());
                        app.needs_redraw = true;
                        return Ok(true);
                    }
                }

                let prompt = app.input.trim().to_string();
                if prompt.is_empty() {
                    return Ok(true);
                }

                // Automatic plan mode trigger for natural language planning requests
                if !app.plan.active && !app.pending_plan_confirmation && !app.pending_plan_request.is_some() {
                    if is_plan_trigger_phrase(&prompt) {
                        app.pending_plan_request = Some(prompt.clone());
                        // Set the goal in the plan state immediately from the user's request
                        // so the pane reflects the actual ask (e.g. birthday cake script) instead of boilerplate.
                        app.plan.goal = prompt.clone();
                        // Start fresh for this request
                        app.plan.success_criteria.clear();
                        app.plan.verification_steps.clear();
                        app.plan.rollback.clear();
                        app.plan.constraints.clear();
                        app.plan.steps.clear();
                        app.plan.current_step = 0;
                        // Also seed the session meta goal so it isn't blank
                        if let Ok(mut ag) = agent.try_lock() {
                            if let Some(s) = &mut ag.session_mut() {
                                s.meta.current_goal = prompt.clone();
                                // Initialize a clean plan template immediately on detecting plan intent.
                                // This way wiki/plan.md starts fresh for this run instead of carrying
                                // over stale content from a previous plan in the same session.
                                let _ = s.write_wiki_file(
                                    "plan.md",
                                    &format!("# Plan\n\n**Goal:** {}\n\n*Template will be expanded on confirmation.*", prompt)
                                );
                                let _ = s.save_meta();
                            }
                        }
                        app.left_committed.push("Do you want to enter plan mode? (y/n)".to_string());
                        app.pending_plan_confirmation = true;
                        app.input = "y".to_string();  // prefill yes; change to n and enter to cancel
                        app.cursor_pos = app.input.len();
                        app.needs_redraw = true;
                        return Ok(true);
                    }
                }

                // In plan mode, detect "proceed" to switch to work mode and show steps
                {
                    let current_mode = if let Ok(ag) = agent.try_lock() { ag.current_agent_mode() } else { String::new() };
                    if current_mode == "plan" {
                        if is_proceed_confirmation(&prompt) {
                            if let Ok(mut ag) = agent.try_lock() {
                                ag.set_agent_mode("work");
                                if let Some(s) = &mut ag.session_mut() {
                                    let _ = s.save_meta();
                                }
                            }
                            app.plan.active = true;
                            if app.plan.steps.is_empty() {
                                let g = app.plan.goal.to_lowercase();
                                // Derive task-appropriate verification (prefer any already set)
                                let mut verif = app.plan.verification_steps.clone();
                                if verif.is_empty() {
                                    if g.contains("python") || g.contains(".py") {
                                        verif = vec!["python3 birthday_cake.py".to_string(), "output contains recognizable ASCII cake".to_string()];
                                    } else if g.contains("c++") || g.contains("cpp") || g.contains("g++") || g.contains("clang") {
                                        verif = vec![
                                            "g++ -std=c++17 -Wall -o program program.cpp".to_string(),
                                            "clang-tidy program.cpp -- -std=c++17".to_string(),
                                            "./program".to_string(),
                                        ];
                                    } else {
                                        verif = vec!["cargo check".to_string(), "cargo clippy -- -D warnings".to_string(), "cargo test".to_string()];
                                    }
                                }
                                app.plan.verification_steps = verif.clone();

                                // Try to extract real steps and (especially) verification from the plan.md the agent wrote during clarification.
                                // This is how language-specific checks (C++ lint etc) end up in the plan instead of stale cargo defaults.
                                let mut extracted_steps: Vec<String> = vec![];
                                let mut extracted_verif: Vec<String> = vec![];
                                if let Ok(ag) = agent.try_lock() {
                                    if let Some(s) = ag.session() {
                                        let plan_content = s.read_wiki_file("plan.md", None, false, 100);
                                        let mut in_verif = false;
                                        for line in plan_content.lines() {
                                            let t = line.trim();
                                            if t.eq_ignore_ascii_case("**verification:**") || t.eq_ignore_ascii_case("verification:") || t.to_lowercase().starts_with("**verification") {
                                                in_verif = true;
                                                continue;
                                            }
                                            if in_verif {
                                                if t.starts_with('#') || t.starts_with("**") && t.contains("step") || t.to_lowercase().starts_with("rollback") {
                                                    in_verif = false;
                                                } else if t.starts_with("- ") || t.starts_with("* ") || t.starts_with("1. ") {
                                                    let v = t.trim_start_matches(|c: char| c == '-' || c == '*' || c == ' ' || (c >= '0' && c <= '9') || c == '.').trim();
                                                    if !v.is_empty() {
                                                        extracted_verif.push(v.to_string());
                                                    }
                                                }
                                            }

                                            if let Some(rest) = t.strip_prefix(|c: char| c.is_ascii_digit()).and_then(|r| r.strip_prefix('.').or_else(|| r.strip_prefix(")"))) {
                                                let desc = rest.trim_start_matches(|c: char| c == ' ' || c == '-' || c == '*').trim();
                                                if !desc.is_empty() && desc.len() > 3 && !desc.to_lowercase().starts_with("verify") {
                                                    extracted_steps.push(desc.to_string());
                                                }
                                            } else if t.starts_with("- ") || t.starts_with("* ") {
                                                let desc = t[2..].trim();
                                                if !desc.is_empty() && desc.len() > 5 && !desc.to_lowercase().contains("rollback") && !desc.to_lowercase().contains("constraint") && !desc.to_lowercase().contains("verify") {
                                                    extracted_steps.push(desc.to_string());
                                                }
                                            }
                                        }
                                    }
                                }

                                // Prefer verification the agent wrote in its plan.md (this is how C++/other language checks get in)
                                if !extracted_verif.is_empty() {
                                    verif = extracted_verif;
                                }
                                app.plan.verification_steps = verif.clone();

                                let mut plan_steps = if !extracted_steps.is_empty() {
                                    extracted_steps.into_iter().take(5).map(|d| crate::app_state::PlanStep {
                                        description: d,
                                        verification: None,
                                        status: crate::app_state::PlanStepStatus::Pending,
                                    }).collect::<Vec<_>>()
                                } else {
                                    // Fallback to meaningful task-aware steps
                                    let step1 = if g.contains("python") || g.contains(".py") || g.contains("script") {
                                        "Write the script / implementation".to_string()
                                    } else if g.contains("test") || g.contains("fix") {
                                        "Implement the fix / changes".to_string()
                                    } else {
                                        "Implement the solution".to_string()
                                    };
                                    let step2 = if g.contains("python") || g.contains("script") {
                                        "Run / execute the script".to_string()
                                    } else {
                                        "Build and test changes".to_string()
                                    };
                                    let step3 = "Verify against success criteria".to_string();
                                    vec![
                                        crate::app_state::PlanStep { description: step1, verification: verif.get(0).cloned(), status: crate::app_state::PlanStepStatus::Pending },
                                        crate::app_state::PlanStep { description: step2, verification: verif.get(1).cloned(), status: crate::app_state::PlanStepStatus::Pending },
                                        crate::app_state::PlanStep { description: step3, verification: verif.get(verif.len().saturating_sub(1)).cloned(), status: crate::app_state::PlanStepStatus::Pending },
                                    ]
                                };

                                if !plan_steps.is_empty() {
                                    plan_steps[0].status = crate::app_state::PlanStepStatus::InProgress;
                                    if let Some(v0) = verif.get(0) {
                                        if plan_steps[0].verification.is_none() {
                                            plan_steps[0].verification = Some(v0.clone());
                                        }
                                    }
                                }

                                app.plan.steps = plan_steps;
                                app.plan.current_step = 0;
                            }
                            // Update wiki with approved plan + steps
                            if let Ok(mut ag) = agent.try_lock() {
                                if let Some(s) = ag.session_mut() {
                                    let verif = app.plan.verification_steps.iter().map(|v| format!("- {}", v)).collect::<Vec<_>>().join("\n");
                                    let steps_str = app.plan.steps.iter().enumerate().map(|(i, st)| {
                                        let v = st.verification.as_deref().unwrap_or("");
                                        format!("{}. {} [verify: {}]", i+1, st.description, v)
                                    }).collect::<Vec<_>>().join("\n");
                                    let plan_text = format!(
                                        "# Plan\n\n**Goal:** {}\n\n**Success Criteria:** {}\n\n**Verification:**\n{}\n\n**Rollback:** {}\n\n**Constraints:** {}\n\n**Steps:**\n{}\n\n## Execution Log\n\n(Added as work proceeds after 'proceed'.)\n",
                                        app.plan.goal, app.plan.success_criteria, verif, app.plan.rollback, app.plan.constraints, steps_str
                                    );
                                    let _ = s.write_wiki_file("plan.md", &plan_text);
                                }
                            }
                            app.left_committed.push("Plan confirmed by user. Switching to work mode. Executing...".to_string());

                            // Also push the final verification into session meta via update_goal so the judge has good criteria
                            if let Ok(mut ag) = agent.try_lock() {
                                if let Some(s) = &mut ag.session_mut() {
                                    let _ = s.update_goal(
                                        &app.plan.goal,
                                        Some(app.plan.verification_steps.clone()),
                                        None,
                                    );
                                }
                            }
                        }
                    }
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
                    pending_plan_confirmation: &mut app.pending_plan_confirmation,
                };

                match crate::input_dispatch::dispatch_slash_command(&prompt, &mut ctx) {
                    crate::input_dispatch::SlashDispatch::AgentPrompt(()) => {
                        // Starting a fresh turn: ensure any prior stop (from Esc abort) is cleared
                        // so the new drive_turn isn't immediately aborted by a stale flag.
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

                        tokio::spawn(async move {
                            let mut ag = agent_c.lock().await;
                            let mode = ag.current_exec_mode();
                            let agent_mode = ag.current_agent_mode();
                            let mut obs = crate::event_loop::TuiObserver {
                                tx: tx_c.clone(),
                                approval_req_tx: appr_c,
                                stop: stop_c.clone(),
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
                                            if stop_c.load(std::sync::atomic::Ordering::SeqCst) {
                                                let _ = tx_c.send(crate::event_loop::UiUpdate::ToolResult {
                                                    name: "system".into(),
                                                    summary: "⏹ Stopped (Esc)".into(),
                                                }).await;
                                                break;
                                            }
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
                                            if stop_c.load(std::sync::atomic::Ordering::SeqCst) {
                                                let _ = tx_c.send(crate::event_loop::UiUpdate::ToolResult {
                                                    name: "system".into(),
                                                    summary: "⏹ Stopped (Esc)".into(),
                                                }).await;
                                                break;
                                            }
                                            let _ = tx_c.send(crate::event_loop::UiUpdate::SuperJudgeBegin).await;
                                            let _ = tx_c.send(crate::event_loop::UiUpdate::ToolResult {
                                                name: "system".into(),
                                                summary: format!("🔍 Super Judge reviewing work (cycle {}/{})", cycle, MAX_SUPER_JUDGE_CYCLES),
                                            }).await;

                                            // Use the headless Super Judge observer (its review may run to completion;
                                            // subsequent nudges and loops will abort on stop).
                                            let mut sj_obs = raven_tui::super_judge::SuperJudgeObserver::new();
                                            let verdict = raven_tui::super_judge::run_super_judge_with_observer(
                                                &mut ag, &last_text, &all_actions, &mut sj_obs,
                                            ).await;

                                            if stop_c.load(std::sync::atomic::Ordering::SeqCst) {
                                                let _ = tx_c.send(crate::event_loop::UiUpdate::ToolResult {
                                                    name: "system".into(),
                                                    summary: "⏹ Stopped (Esc)".into(),
                                                }).await;
                                                break;
                                            }

                                            match verdict {
                                                raven_tui::super_judge::SuperJudgeVerdict::Complete { note } => {
                                                    let _ = tx_c.send(crate::event_loop::UiUpdate::ToolResult {
                                                        name: "system".into(),
                                                        summary: format!("🔍 Super Judge: WORK_COMPLETE — {}", note),
                                                    }).await;
                                                    // Advance plan if active
                                                    break;
                                                }
                                                raven_tui::super_judge::SuperJudgeVerdict::Continue { feedback } => {
                                                    let _ = tx_c.send(crate::event_loop::UiUpdate::ToolResult {
                                                        name: "system".into(),
                                                        summary: "🔍 Super Judge: NEEDS_WORK — nudging agent".to_string(),
                                                    }).await;
                                                    if stop_c.load(std::sync::atomic::Ordering::SeqCst) {
                                                        let _ = tx_c.send(crate::event_loop::UiUpdate::ToolResult {
                                                            name: "system".into(),
                                                            summary: "⏹ Stopped (Esc)".into(),
                                                        }).await;
                                                        break;
                                                    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_various_plan_triggers() {
        assert!(is_plan_trigger_phrase("come up with a plan to write a script"));
        assert!(is_plan_trigger_phrase("let's plan this out"));
        assert!(is_plan_trigger_phrase("make a plan for the refactor"));
        assert!(is_plan_trigger_phrase("plan the implementation"));
        assert!(is_plan_trigger_phrase("create a plan for this task"));
        assert!(is_plan_trigger_phrase("what's the plan for the work"));
        assert!(is_plan_trigger_phrase("plan for this change"));
        // The broad "plan" + context
        assert!(is_plan_trigger_phrase("I want to plan the task"));
        assert!(is_plan_trigger_phrase("plan out the new feature"));
        // Negative
        assert!(!is_plan_trigger_phrase("what is your plan for dinner"));
        assert!(!is_plan_trigger_phrase("just talk about the plan"));
    }

    #[test]
    fn detects_proceed_variants() {
        assert!(is_proceed_confirmation("proceed"));
        assert!(is_proceed_confirmation("let's proceed with the plan"));
        assert!(is_proceed_confirmation("go ahead"));
        assert!(is_proceed_confirmation("yes"));
        assert!(is_proceed_confirmation("let's do it"));
        assert!(is_proceed_confirmation("do it"));
        assert!(is_proceed_confirmation("go for it"));
        assert!(is_proceed_confirmation("confirmed"));
        assert!(is_proceed_confirmation("start executing"));
        // Ambiguous should not be over-eager in this fn (the agent may still ask for clarity)
        assert!(!is_proceed_confirmation("maybe"));
        assert!(!is_proceed_confirmation("sounds good"));
    }
}


