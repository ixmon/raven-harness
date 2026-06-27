//! Slash-command dispatch and unified navigation key handling (glm.md refactor).

use raven_tui::agent::Agent;
use raven_tui::config::Config;
use crate::keystore::Keystore;
use crate::search::{run_search, SearchState};
use crate::settings_modal::SettingsModal;
use crate::tui_render::Pane;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Result of handling a user prompt that may be a slash command.
pub enum SlashDispatch {
    /// Normal prompt — send to the agent.
    AgentPrompt(()),
    /// Command handled; UI should refresh.
    Handled,
    /// Exit the TUI.
    Quit,
}

pub struct SlashContext<'a> {
    pub left_committed: &'a mut Vec<String>,
    pub trace_lines: &'a mut Vec<String>,
    pub current_response: &'a str,
    pub current_thinking: &'a str,
    pub input: &'a mut String,
    pub cursor_pos: &'a mut usize,
    pub slash_commands: &'a [SlashCommand],
    pub slash_selected: &'a mut usize,
    pub mode_menu_active: &'a mut bool,
    pub selected_mode_idx: &'a mut usize,
    pub settings: &'a mut SettingsModal,
    pub search: &'a mut SearchState,
    pub focused_pane: Pane,
    pub left_scroll: &'a mut u16,
    pub right_scroll: &'a mut u16,
    pub left_follow_output: &'a mut bool,
    pub right_follow_output: &'a mut bool,
    pub last_left_line_count: u16,
    pub last_right_line_count: u16,
    pub last_left_area_h: u16,
    pub last_right_area_h: u16,
    pub config: &'a Config,
    pub keystore: &'a Keystore,
    pub agent: &'a Arc<Mutex<Agent>>,
}

fn clear_slash_input(ctx: &mut SlashContext<'_>) {
    ctx.input.clear();
    *ctx.cursor_pos = 0;
}

pub fn filtered_slash_commands<'a>(commands: &'a [SlashCommand], input: &str) -> Vec<&'a SlashCommand> {
    if !input.starts_with('/') {
        return vec![];
    }
    let prefix = &input[1..].to_lowercase();
    commands
        .iter()
        .filter(|cmd| prefix.is_empty() || cmd.name.starts_with(prefix))
        .collect()
}

/// Dispatch a `/command` prompt. Returns `AgentPrompt` if the text should go to the model.
pub fn dispatch_slash_command(prompt: &str, ctx: &mut SlashContext<'_>) -> SlashDispatch {
    if !prompt.starts_with('/') {
        return SlashDispatch::AgentPrompt(());
    }

    let filtered = filtered_slash_commands(ctx.slash_commands, prompt);

    let name = if !filtered.is_empty() {
        let idx = (*ctx.slash_selected).min(filtered.len().saturating_sub(1));
        filtered[idx].name.to_string()
    } else {
        prompt
            .trim_start_matches('/')
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_lowercase()
    };

    match name.as_str() {
        "help" | "?" => {
            ctx.left_committed.push(HELP_TEXT.to_string());
            scroll_left(ctx.left_committed);
            clear_slash_input(ctx);
            *ctx.slash_selected = 0;
            SlashDispatch::Handled
        }
        "clear" => {
            if let Ok(mut ag) = ctx.agent.try_lock() {
                ag.reset();
            }
            ctx.left_committed.clear();
            ctx.left_committed.push("Conversation cleared.".to_string());
            scroll_left(ctx.left_committed);
            clear_slash_input(ctx);
            *ctx.slash_selected = 0;
            SlashDispatch::Handled
        }
        "clear-trace" => {
            ctx.trace_lines.clear();
            clear_slash_input(ctx);
            *ctx.slash_selected = 0;
            SlashDispatch::Handled
        }
        "reset" => {
            if let Ok(mut ag) = ctx.agent.try_lock() {
                ag.reset();
            }
            ctx.left_committed.clear();
            ctx.left_committed
                .push("Conversation reset (persistent session kept).".to_string());
            scroll_left(ctx.left_committed);
            clear_slash_input(ctx);
            *ctx.slash_selected = 0;
            SlashDispatch::Handled
        }
        "status" => {
            let (endpoint_label, model, base_url, workspace, mode_label) =
                if let Ok(ag) = ctx.agent.try_lock() {
                    let cfg = ag.current_config();
                    let label = ctx
                        .settings
                        .endpoints
                        .get(ctx.settings.active_endpoint_idx)
                        .map(|e| e.label.clone())
                        .unwrap_or_else(|| "session".to_string());
                    (
                        label,
                        cfg.model.clone(),
                        cfg.base_url.clone(),
                        cfg.workspace.display().to_string(),
                        ag.current_exec_mode().label().to_string(),
                    )
                } else {
                    (
                        "unknown".to_string(),
                        ctx.config.model.clone(),
                        ctx.config.base_url.clone(),
                        ctx.config.workspace.display().to_string(),
                        "unknown".to_string(),
                    )
                };
            let status = format!(
                "Session status\n  Endpoint:  {}\n  Model:     {}\n  Base URL:  {}\n  Workspace: {}\n  Exec Mode: {}\n  History:   {} entries",
                endpoint_label,
                model,
                base_url,
                workspace,
                mode_label,
                ctx.left_committed.len()
            );
            ctx.left_committed.push(status);
            scroll_left(ctx.left_committed);
            clear_slash_input(ctx);
            *ctx.slash_selected = 0;
            SlashDispatch::Handled
        }
        "mode" => {
            *ctx.mode_menu_active = true;
            *ctx.selected_mode_idx = 0;
            if let Ok(ag) = ctx.agent.try_lock() {
                if let Some(s) = &ag.session() {
                    *ctx.selected_mode_idx = match s.meta.exec_approval_mode {
                        raven_tui::session::ExecApprovalMode::Babysitter => 0,
                        raven_tui::session::ExecApprovalMode::SpringBreak => 1,
                        raven_tui::session::ExecApprovalMode::Vegas => 2,
                        raven_tui::session::ExecApprovalMode::Thunderdome => 3,
                    };
                }
            }
            clear_slash_input(ctx);
            *ctx.slash_selected = 0;
            ctx.left_committed.push(
                "Use ↑/↓ to select execution mode, Enter to confirm, Esc to cancel.".to_string(),
            );
            scroll_left(ctx.left_committed);
            SlashDispatch::Handled
        }
        "settings" => {
            let fallback = if let Ok(ag) = ctx.agent.try_lock() {
                ag.current_config().clone()
            } else {
                ctx.config.clone()
            };
            ctx.settings.open(ctx.keystore, &fallback);
            clear_slash_input(ctx);
            *ctx.slash_selected = 0;
            SlashDispatch::Handled
        }
        "search" => {
            let query = prompt
                .trim_start_matches('/')
                .split_whitespace()
                .skip(1)
                .collect::<Vec<_>>()
                .join(" ");
            if query.is_empty() {
                ctx.search.active = true;
                ctx.search.query.clear();
                ctx.search.match_lines.clear();
                clear_slash_input(ctx);
            } else {
                ctx.search.active = true;
                ctx.search.query = query;
                if let Some(line) = run_search(
                    ctx.search,
                    ctx.left_committed,
                    ctx.current_response,
                    ctx.trace_lines,
                    ctx.current_thinking,
                    ctx.focused_pane,
                ) {
                    apply_search_scroll(ctx, line);
                }
                ctx.left_committed.push(format!(
                    "🔍 Search '{}': {} match(es) in {:?} pane",
                    ctx.search.query,
                    ctx.search.match_lines.len(),
                    ctx.search.pane
                ));
                scroll_left(ctx.left_committed);
                clear_slash_input(ctx);
            }
            *ctx.slash_selected = 0;
            SlashDispatch::Handled
        }
        "quit" | "exit" | "q" => SlashDispatch::Quit,
        _ => {
            ctx.left_committed.push(format!(
                "⚠ Unknown command: {}. Type /help to list commands.",
                prompt
            ));
            scroll_left(ctx.left_committed);
            clear_slash_input(ctx);
            *ctx.slash_selected = 0;
            SlashDispatch::Handled
        }
    }
}

fn scroll_left(left: &mut Vec<String>) {
    // Caller sets follow/scroll — this helper only pushes content.
    let _ = left;
}

pub struct SlashCommand {
    pub name: &'static str,
    pub desc: &'static str,
}

pub fn default_slash_commands() -> Vec<SlashCommand> {
    vec![
        SlashCommand {
            name: "help",
            desc: "Show available / commands",
        },
        SlashCommand {
            name: "clear",
            desc: "Clear conversation history",
        },
        SlashCommand {
            name: "clear-trace",
            desc: "Clear only the trace pane",
        },
        SlashCommand {
            name: "reset",
            desc: "Reset conversation (keeps goals/session)",
        },
        SlashCommand {
            name: "status",
            desc: "Show current config and session info",
        },
        SlashCommand {
            name: "mode",
            desc: "Change execution approval mode",
        },
        SlashCommand {
            name: "settings",
            desc: "Manage inference endpoints",
        },
        SlashCommand {
            name: "search",
            desc: "Search conversation or trace pane",
        },
        SlashCommand {
            name: "quit",
            desc: "Exit the TUI",
        },
    ]
}

fn apply_search_scroll(ctx: &mut SlashContext<'_>, line: usize) {
    let content_h = match ctx.search.pane {
        Pane::Left => ctx.last_left_area_h.saturating_sub(2),
        Pane::Right => ctx.last_right_area_h.saturating_sub(2),
    };
    let scroll = crate::search::scroll_to_line(line, content_h);
    match ctx.search.pane {
        Pane::Left => {
            *ctx.left_scroll = scroll.min(ctx.last_left_line_count.saturating_sub(1));
            *ctx.left_follow_output = false;
        }
        Pane::Right => {
            *ctx.right_scroll = scroll.min(ctx.last_right_line_count.saturating_sub(1));
            *ctx.right_follow_output = false;
        }
    }
}


pub const HELP_TEXT: &str = "\
Available commands:
/help          Show this help
/clear         Clear the conversation pane
/clear-trace   Clear the right trace pane
/reset         Reset conversation memory (session goals stay)
/status        Show endpoint, model, workspace
/mode          Change execution approval mode (Babysitter / Spring Break / Vegas / Thunderdome)
/settings      Manage inference endpoints (add/switch/edit/delete)
/search        Search conversation or trace (or Ctrl-F)
/quit or /exit Quit the TUI

Keybindings:
  Tab          Switch focus between conversation and trace panes
  ↑/↓          Scroll the focused pane (PgUp/PgDn for faster scroll)
  Ctrl+↑/↓     Recall previous prompts
  Ctrl+F       Open search bar (n/N or Ctrl-N/P for next/prev match)
  Shift+Insert / Ctrl+V   Paste (also terminal bracketed paste)
  Shift+select, Ctrl+Insert  Copy via terminal selection (SSH-friendly)
  Y / N        Approve or deny sandbox actions
  Esc          Stop agent (while processing) or cancel menus

While agent is running:
  Enter        Queue interject (applies before next tool round)
  Ctrl+Enter   Send interject now (stops current inference)
  Shift+Enter  Newline in input

Tip: type / then use ↑↓ to browse, Tab to complete.";

/// Apply settings-side-effect actions to the App UI fields.
pub fn apply_settings_actions(
    actions: Vec<crate::settings_modal::SettingsAction>,
    left_committed: &mut Vec<String>,
    trace_lines: &mut Vec<String>,
    display_model: &mut String,
    display_budget: &mut raven_tui::config::ContextBudget,
    settings: &mut SettingsModal,
) {
    use crate::settings_modal::SettingsAction;

    for action in actions {
        match action {
            SettingsAction::Redraw => {}
            SettingsAction::Close => settings.active = false,
            SettingsAction::Notify(msg) => {
                left_committed.push(msg);
            }
            SettingsAction::Trace(msg) => {
                trace_lines.push(msg);
            }
            SettingsAction::DisplayUpdate { model, budget } => {
                *display_model = model;
                *display_budget = budget;
            }
            SettingsAction::ActiveIdx(idx) => {
                settings.active_endpoint_idx = idx;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use raven_tui::agent::Agent;
    use raven_tui::config::{Config, ContextBudget};
    use crate::settings_modal::SettingsModal;
    use crate::tui_render::Pane;
    use std::path::PathBuf;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    #[test]
    fn slash_commands_reset_cursor_after_clearing_input() {
        let config = Config {
            base_url: "http://127.0.0.1:8080/v1".to_string(),
            model: "test".to_string(),
            api_key: None,
            workspace: PathBuf::from("/tmp"),
            temperature: 0.2,
            max_tokens: 4096,
            max_rounds: 10,
            prebuilt_session: None,
            context_budget: ContextBudget::from_context_tokens(8192, 10),
            tool_backend: raven_tui::tools::ToolBackend::default(),
            tools_enabled: true,
            enable_judge: false,
            flags: raven_tui::runtime::RuntimeFlags::default(),
            harness: raven_tui::runtime::EvalHarness::default(),
        };
        let agent = Arc::new(Mutex::new(Agent::new(
            config.clone(),
            raven_tui::chat_backend::ChatBackend::http(config.clone()),
        )));
        let ks_path = std::env::temp_dir().join(format!(
            "raven_slash_test_{}.json",
            std::process::id()
        ));
        let ks = Keystore::load_or_create(&ks_path).expect("keystore");
        let mut input = "/status".to_string();
        let mut cursor_pos = 45;
        let mut left = Vec::new();
        let mut trace = Vec::new();
        let slash_commands = default_slash_commands();
        let mut slash_selected = 0;
        let mut mode_menu_active = false;
        let mut selected_mode_idx = 0;
        let mut settings = SettingsModal::inactive();
        let mut search = Default::default();
        let mut left_scroll = 0;
        let mut right_scroll = 0;
        let mut left_follow_output = true;
        let mut right_follow_output = true;

        let mut ctx = SlashContext {
            left_committed: &mut left,
            trace_lines: &mut trace,
            current_response: "",
            current_thinking: "",
            input: &mut input,
            cursor_pos: &mut cursor_pos,
            slash_commands: &slash_commands,
            slash_selected: &mut slash_selected,
            mode_menu_active: &mut mode_menu_active,
            selected_mode_idx: &mut selected_mode_idx,
            settings: &mut settings,
            search: &mut search,
            focused_pane: Pane::Left,
            left_scroll: &mut left_scroll,
            right_scroll: &mut right_scroll,
            left_follow_output: &mut left_follow_output,
            right_follow_output: &mut right_follow_output,
            last_left_line_count: 0,
            last_right_line_count: 0,
            last_left_area_h: 24,
            last_right_area_h: 24,
            config: &config,
            keystore: &ks,
            agent: &agent,
        };
        let result = dispatch_slash_command("/status", &mut ctx);
        assert!(matches!(result, SlashDispatch::Handled));
        assert!(input.is_empty());
        assert_eq!(cursor_pos, 0);
    }
}