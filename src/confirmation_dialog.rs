//! Reusable Y/N confirmation overlays (tool approval, plan entry, etc.).

use ratatui::style::Color;
use tokio::sync::oneshot;

/// A blocking confirmation shown as a modal overlay (same chrome as babysitter approval).
#[derive(Debug)]
pub enum ConfirmationDialog {
    ToolApproval {
        description: String,
        responder: oneshot::Sender<bool>,
    },
    PlanEntry {
        goal: String,
    },
}

/// Visual properties for [`draw_confirmation_modal`].
pub struct ConfirmationModalView<'a> {
    pub title: &'a str,
    pub border_color: Color,
    pub headline: String,
    pub headline_suffix: &'a str,
    pub detail: String,
}

impl ConfirmationDialog {
    pub fn view(&self) -> ConfirmationModalView<'_> {
        match self {
            ConfirmationDialog::ToolApproval { description, .. } => {
                let (kind, detail) = description
                    .split_once(": ")
                    .unwrap_or(("action", description.as_str()));
                let border_color = if kind.starts_with("write") || kind.starts_with("patch") {
                    Color::Rgb(0xff, 0x60, 0x60) // red — destructive filesystem op
                } else if kind.starts_with("exec") || kind.starts_with("Install") {
                    Color::Yellow // yellow — command execution
                } else {
                    Color::Cyan // informational (update_goal, etc.)
                };
                ConfirmationModalView {
                    title: " Action Approval ",
                    border_color,
                    headline: kind.to_string(),
                    headline_suffix: " — sandbox approval needed",
                    detail: detail.to_string(),
                }
            }
            ConfirmationDialog::PlanEntry { goal } => ConfirmationModalView {
                title: " Plan Mode ",
                border_color: Color::Rgb(0xff, 0xc0, 0x40),
                headline: "Enter plan mode?".to_string(),
                headline_suffix: "",
                detail: goal.clone(),
            },
        }
    }

    pub fn is_plan_entry(&self) -> bool {
        matches!(self, ConfirmationDialog::PlanEntry { .. })
    }
}

/// Result of handling a key while a confirmation modal is open.
#[derive(Debug, PartialEq, Eq)]
pub enum ConfirmationKeyOutcome {
    NotHandled,
    Handled,
    PlanEntry { goal: String, confirmed: bool },
}