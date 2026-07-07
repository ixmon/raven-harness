//! Plan mode side-pane rendering.

use crate::plan_state::{PlanLoopPhase, PlanState, PlanStepStatus};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Paragraph, Wrap},
    Frame,
};

const PLAN_ORANGE: Color = Color::Rgb(0xff, 0xc0, 0x40);
const PLAN_GREEN: Color = Color::Rgb(0x60, 0xc0, 0x60);

pub fn draw_plan_pane(f: &mut Frame, area: Rect, plan: &PlanState) {
    let is_approved = !plan.steps.is_empty();
    let pane_color = if is_approved { PLAN_GREEN } else { PLAN_ORANGE };
    let title_text = if is_approved {
        " Plan (approved) "
    } else {
        " Plan "
    };
    let block = Block::default()
        .title(Span::styled(
            title_text,
            Style::default()
                .fg(pane_color)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(pane_color))
        .style(Style::default().bg(Color::Black));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines: Vec<Line> = vec![];

    lines.push(Line::from(vec![
        Span::styled("Goal: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            if plan.goal.is_empty() {
                "(gathering from user)"
            } else {
                &plan.goal
            },
            Style::default().fg(Color::White),
        ),
    ]));
    if !plan.success_criteria.is_empty() {
        lines.push(Line::from(vec![
            Span::styled(
                "Success Criteria: ",
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(
                &plan.success_criteria,
                Style::default().fg(Color::Rgb(0xcc, 0xcc, 0xdd)),
            ),
        ]));
    }
    if !plan.verification_steps.is_empty() {
        lines.push(Line::from(Span::styled(
            "Verification:",
            Style::default().fg(Color::DarkGray),
        )));
        for v in plan.verification_steps.iter().take(2) {
            lines.push(Line::from(Span::styled(
                format!("  • {}", v),
                Style::default().fg(Color::Gray),
            )));
        }
    }
    if !plan.rollback.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("Rollback: ", Style::default().fg(Color::DarkGray)),
            Span::styled(&plan.rollback, Style::default().fg(Color::Yellow)),
        ]));
    }
    if !plan.constraints.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("Constraints: ", Style::default().fg(Color::DarkGray)),
            Span::styled(&plan.constraints, Style::default().fg(Color::Gray)),
        ]));
    }

    lines.push(Line::from(""));

    if plan.steps.is_empty() {
        let spins = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        let slow = spins[(plan.spinner_tick / 4) % spins.len()];
        match plan.loop_phase {
            PlanLoopPhase::FetchingQuestion | PlanLoopPhase::FetchingProposal => {
                lines.push(Line::from(Span::styled(
                    format!("{slow} Planning… (JSON loop)"),
                    Style::default().fg(Color::DarkGray),
                )));
            }
            PlanLoopPhase::AwaitingUserAnswer => {
                if let Some(q) = &plan.pending_question {
                    lines.push(Line::from(Span::styled(
                        format!("❓ {}", q.prompt),
                        Style::default().fg(Color::Rgb(0xff, 0xcc, 0x66)),
                    )));
                    for (i, opt) in q.options.iter().enumerate() {
                        let rec = q
                            .recommend
                            .as_deref()
                            .is_some_and(|r| r == opt.id)
                            .then_some(" ★");
                        lines.push(Line::from(Span::styled(
                            format!("  {}. {}{}", i + 1, opt.label, rec.unwrap_or_default()),
                            Style::default().fg(Color::Gray),
                        )));
                    }
                }
            }
            PlanLoopPhase::AwaitingProceedConsent => {
                lines.push(Line::from(Span::styled(
                    "📋 Review proposal in chat — ready to proceed?",
                    Style::default().fg(Color::Rgb(0xcc, 0xff, 0xcc)),
                )));
            }
            _ => {
                lines.push(Line::from(Span::styled(
                    format!("{slow} Steps: (to be determined)"),
                    Style::default().fg(Color::DarkGray),
                )));
            }
        }
    } else {
        let spins = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        let spin = spins[plan.spinner_tick % spins.len()];
        let progress = if !plan.steps.is_empty() && plan.current_step < plan.steps.len() {
            let st = &plan.steps[plan.current_step];
            let observe = plan
                .pending_observe_prompt
                .as_deref()
                .map(|p| format!(" — waiting: {}", p))
                .unwrap_or_default();
            format!(
                "{} Step {}/{}{}{}",
                spin,
                plan.current_step + 1,
                plan.steps.len(),
                st.description,
                observe
            )
        } else if !plan.steps.is_empty() {
            "✓ Plan complete".to_string()
        } else {
            format!("{} Preparing plan...", spin)
        };
        lines.push(Line::from(Span::styled(
            progress,
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )));

        if !plan.steps.is_empty() {
            let total = plan.steps.len();
            let done = plan
                .steps
                .iter()
                .filter(|s| s.status == PlanStepStatus::Done)
                .count();
            let mut pct = done.saturating_mul(100) / total.max(1);
            if plan.current_step >= total || done >= total {
                pct = 100;
            }
            let bar_width = 20;
            let filled = pct * bar_width / 100;
            let bar = format!(
                "[{}{}] {}%",
                "=".repeat(filled),
                " ".repeat(bar_width - filled),
                pct
            );
            lines.push(Line::from(Span::styled(
                bar,
                Style::default().fg(pane_color),
            )));
        }

        for (i, st) in plan.steps.iter().enumerate().take(5) {
            let mark = if i == plan.current_step {
                "▶ "
            } else {
                match st.status {
                    PlanStepStatus::Done => "✓ ",
                    PlanStepStatus::Failed => "✗ ",
                    _ => "  ",
                }
            };
            let tier = st
                .tier
                .map(|t| format!(" [{}]", t.pane_label()))
                .unwrap_or_default();
            let v = st
                .verification
                .as_deref()
                .or(st.observe_prompt.as_deref())
                .map(|v| format!(" ({})", v))
                .unwrap_or_default();
            lines.push(Line::from(Span::styled(
                format!("{}{}{}{}", mark, st.description, tier, v),
                Style::default().fg(Color::Gray),
            )));
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Stored in: wiki/plan.md (editable outside)",
        Style::default().fg(Color::DarkGray),
    )));

    let para = Paragraph::new(Text::from(lines)).wrap(Wrap { trim: true });
    f.render_widget(para, inner);
}