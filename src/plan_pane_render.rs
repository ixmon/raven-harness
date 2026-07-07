//! Plan mode side-pane rendering.

use crate::plan_state::{PlanLoopPhase, PlanState, PlanStepStatus};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, LineGauge, Paragraph, Widget, Wrap},
    Frame,
};

const PLAN_ORANGE: Color = Color::Rgb(0xff, 0xc0, 0x40);
const PLAN_GREEN: Color = Color::Rgb(0x60, 0xc0, 0x60);
/// Progress fill — amber, distinct from the approved pane border green.
const PLAN_PROGRESS_FILL: Color = Color::Rgb(0xe0, 0xa8, 0x30);
const PLAN_PROGRESS_TRACK: Color = Color::Rgb(0x2a, 0x2a, 0x34);

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

    let mut lines_top: Vec<Line> = vec![];
    let mut lines_bottom: Vec<Line> = vec![];
    let mut gauge_pct: Option<u16> = None;

    lines_top.push(Line::from(vec![
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
        lines_top.push(Line::from(vec![
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
        lines_top.push(Line::from(Span::styled(
            "Verification:",
            Style::default().fg(Color::DarkGray),
        )));
        for v in plan.verification_steps.iter().take(2) {
            lines_top.push(Line::from(Span::styled(
                format!("  • {}", v),
                Style::default().fg(Color::Gray),
            )));
        }
    }
    if !plan.rollback.is_empty() {
        lines_top.push(Line::from(vec![
            Span::styled("Rollback: ", Style::default().fg(Color::DarkGray)),
            Span::styled(&plan.rollback, Style::default().fg(Color::Yellow)),
        ]));
    }
    if !plan.constraints.is_empty() {
        lines_top.push(Line::from(vec![
            Span::styled("Constraints: ", Style::default().fg(Color::DarkGray)),
            Span::styled(&plan.constraints, Style::default().fg(Color::Gray)),
        ]));
    }

    lines_top.push(Line::from(""));

    if plan.steps.is_empty() {
        let spins = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        let slow = spins[(plan.spinner_tick / 4) % spins.len()];
        match plan.loop_phase {
            PlanLoopPhase::FetchingQuestion | PlanLoopPhase::FetchingProposal => {
                lines_top.push(Line::from(Span::styled(
                    format!("{slow} Planning… (JSON loop)"),
                    Style::default().fg(Color::DarkGray),
                )));
            }
            PlanLoopPhase::AwaitingUserAnswer => {
                if let Some(q) = &plan.pending_question {
                    lines_top.push(Line::from(Span::styled(
                        format!("❓ {}", q.prompt),
                        Style::default().fg(Color::Rgb(0xff, 0xcc, 0x66)),
                    )));
                    for (i, opt) in q.options.iter().enumerate() {
                        let rec = q
                            .recommend
                            .as_deref()
                            .is_some_and(|r| r == opt.id)
                            .then_some(" ★");
                        lines_top.push(Line::from(Span::styled(
                            format!("  {}. {}{}", i + 1, opt.label, rec.unwrap_or_default()),
                            Style::default().fg(Color::Gray),
                        )));
                    }
                }
            }
            PlanLoopPhase::AwaitingProceedConsent => {
                lines_top.push(Line::from(Span::styled(
                    "📋 Review proposal in chat — ready to proceed?",
                    Style::default().fg(Color::Rgb(0xcc, 0xff, 0xcc)),
                )));
            }
            _ => {
                lines_top.push(Line::from(Span::styled(
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
                "{} Step {}/{} {}{}",
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
        lines_top.push(Line::from(Span::styled(
            progress,
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )));

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
        gauge_pct = Some(pct as u16);

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
            lines_bottom.push(Line::from(Span::styled(
                format!("{}{}{}{}", mark, st.description, tier, v),
                Style::default().fg(Color::Gray),
            )));
        }
    }

    lines_bottom.push(Line::from(""));
    lines_bottom.push(Line::from(Span::styled(
        "Stored in: wiki/plan.md (editable outside)",
        Style::default().fg(Color::DarkGray),
    )));

    if let Some(pct) = gauge_pct {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(0),
                Constraint::Length(1),
                Constraint::Min(0),
            ])
            .split(inner);

        let top_para = Paragraph::new(Text::from(lines_top)).wrap(Wrap { trim: true });
        f.render_widget(top_para, chunks[0]);

        let done = plan
            .steps
            .iter()
            .filter(|s| s.status == PlanStepStatus::Done)
            .count();
        let gauge_label = format!(" {pct}% ({done}/{})", plan.steps.len());
        // Inset + partial width so the bar reads as inline progress, not a second border.
        let gauge_area = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(2),
                Constraint::Percentage(31),
                Constraint::Min(0),
            ])
            .split(chunks[1])[1];
        let gauge = LineGauge::default()
            .ratio(f64::from(pct) / 100.0)
            .label(Line::from(Span::styled(
                gauge_label,
                Style::default().fg(Color::Rgb(0xbb, 0xbb, 0xcc)),
            )))
            .filled_style(Style::default().fg(PLAN_PROGRESS_FILL))
            .unfilled_style(Style::default().fg(PLAN_PROGRESS_TRACK))
            .line_set(ratatui::symbols::line::NORMAL);
        gauge.render(gauge_area, f.buffer_mut());

        let bottom_para = Paragraph::new(Text::from(lines_bottom)).wrap(Wrap { trim: true });
        f.render_widget(bottom_para, chunks[2]);
    } else {
        let mut lines = lines_top;
        lines.extend(lines_bottom);
        let para = Paragraph::new(Text::from(lines)).wrap(Wrap { trim: true });
        f.render_widget(para, inner);
    }
}