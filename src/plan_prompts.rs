//! Plan-related user-facing strings: execution prompts, status, wiki templates.

use crate::plan_state::{PlanState, PlanStep, PlanStepStatus, PlanStepTier};
use raven_tui::plan_execution;
use raven_tui::plan_md::{self, PlanStepData};
use std::path::Path;

pub fn format_plan_steps_json_block(steps: &[PlanStep]) -> String {
    let data: Vec<PlanStepData> = steps.iter().map(crate::plan_sync::plan_step_to_data).collect();
    plan_md::format_plan_steps_json_block(&data)
}

/// User message when starting work mode after plan approval (includes step list).
pub fn format_plan_execution_user_prompt(plan: &PlanState, workspace: &Path) -> String {
    let mut lines = vec![
        "Execute the approved plan.".to_string(),
        format!("Goal: {}", plan.goal),
    ];
    if !plan.success_criteria.is_empty() {
        lines.push(format!("Success criteria: {}", plan.success_criteria));
    }
    let steps_data: Vec<PlanStepData> = plan
        .steps
        .iter()
        .map(crate::plan_sync::plan_step_to_data)
        .collect();
    let workdir = plan
        .project_workdir
        .clone()
        .or_else(|| plan_execution::detect_project_workdir(workspace, &steps_data));
    lines.push(String::new());
    lines.push(plan_execution::format_deliverable_location_section(
        workspace,
        workdir.as_deref(),
    ));
    if !plan.steps.is_empty() {
        lines.push(String::new());
        lines.push(
            "Approved steps (one at a time; call complete_plan_step when each is done):".to_string(),
        );
        for (i, step) in plan.steps.iter().enumerate() {
            let n = i + 1;
            let tier = step.tier.map(|t| t.as_str()).unwrap_or("exec");
            let verify = step
                .verification
                .as_deref()
                .or(step.observe_prompt.as_deref())
                .unwrap_or("—");
            let cur = if i == plan.current_step {
                " ← CURRENT"
            } else {
                ""
            };
            lines.push(format!(
                "  {n}. {} [{tier}: {verify}]{cur}",
                step.description
            ));
        }
    }
    lines.push(String::new());
    lines.push(
        "Canonical plan: session wiki plan.md (read with wiki=true). \
         Follow the approved steps — they define which paths to use."
            .to_string(),
    );
    let step_num = plan.current_step.saturating_add(1);
    lines.push(format!("Begin with step {step_num}."));
    lines.join("\n")
}

/// Format plan pane state for `/plan status`.
pub fn format_plan_status(plan: &PlanState, agent_mode: &str) -> String {
    let mut lines = vec![format!("Plan status (run mode: {})", agent_mode)];
    if !plan.goal.is_empty() {
        lines.push(format!("  Goal: {}", plan.goal));
    }
    match plan
        .project_workdir
        .as_deref()
        .map(str::trim)
        .filter(|w| !w.is_empty() && *w != ".")
    {
        Some(wd) => lines.push(format!(
            "  Deliverables: {}/ (isolated under workspace)",
            wd.trim_end_matches('/')
        )),
        None => lines.push(
            "  Deliverables: workspace root (not confined to a subdir)".to_string(),
        ),
    }
    if !plan.success_criteria.is_empty() {
        lines.push(format!("  Success criteria: {}", plan.success_criteria));
    }
    if !plan.verification_steps.is_empty() {
        lines.push("  Verification:".to_string());
        for v in plan.verification_steps.iter().take(5) {
            lines.push(format!("    • {}", v));
        }
    }
    if plan.steps.is_empty() {
        lines.push("  Steps: (not approved yet)".to_string());
    } else {
        lines.push(format!(
            "  Steps: {}/{} complete (current: {})",
            plan.steps.iter().filter(|s| s.status == PlanStepStatus::Done).count(),
            plan.steps.len(),
            plan.current_step + 1
        ));
        for (i, st) in plan.steps.iter().enumerate().take(5) {
            let tier = st
                .tier
                .map(|t| t.as_str())
                .unwrap_or("exec");
            lines.push(format!("    {}. {} [{}]", i + 1, st.description, tier));
        }
    }
    if let Some(p) = &plan.pending_observe_prompt {
        lines.push(format!("  Awaiting observation: {}", p));
    }
    lines.join("\n")
}

fn format_verification_list(verification: &[String]) -> String {
    verification
        .iter()
        .map(|v| format!("- {}", v))
        .collect::<Vec<_>>()
        .join("\n")
}

fn plan_steps_section_skeleton() -> String {
    format!(
        "## Steps\n{}\n",
        format_plan_steps_json_block(&[
            PlanStep {
                description: "(e.g. Create project layout)".to_string(),
                verification: Some("file_exists:CMakeLists.txt".to_string()),
                tier: Some(PlanStepTier::Check),
                note: None,
                observe_prompt: None,
                status: PlanStepStatus::Pending,
            },
            PlanStep {
                description: "(e.g. Implement core module)".to_string(),
                verification: Some("grep:fn main:src/main.rs".to_string()),
                tier: Some(PlanStepTier::Check),
                note: None,
                observe_prompt: None,
                status: PlanStepStatus::Pending,
            },
            PlanStep {
                description: "(e.g. Compile and test)".to_string(),
                verification: Some("cargo check".to_string()),
                tier: Some(PlanStepTier::Exec),
                note: None,
                observe_prompt: None,
                status: PlanStepStatus::Pending,
            },
        ])
    )
}

pub fn wiki_template_on_entry(plan: &PlanState) -> String {
    format!(
        "# Plan\n\n**Goal:** {}\n\n**Success Criteria:** {}\n\n## Verification\n{}\n\n**Rollback:** {}\n\n**Constraints:** {}\n\n{}\n## Notes\n\n(Agent will refine this during clarification. Final approved version written on 'proceed'.)\n",
        plan.goal,
        plan.success_criteria,
        format_verification_list(&plan.verification_steps),
        plan.rollback,
        plan.constraints,
        plan_steps_section_skeleton()
    )
}

pub fn wiki_template_on_trigger(goal: &str) -> String {
    format!(
        "# Plan\n\n**Goal:** {}\n\n*Template will be expanded on confirmation.*",
        goal
    )
}

fn format_step_human_summary(st: &PlanStep, index: usize) -> String {
    let tier = st
        .tier
        .map(|t| t.as_str())
        .unwrap_or("exec");
    let verify = st
        .verification
        .as_deref()
        .or(st.observe_prompt.as_deref())
        .unwrap_or("");
    format!("{}. {} [{}: {}]", index + 1, st.description, tier, verify)
}

pub fn wiki_template_approved(plan: &PlanState) -> String {
    let verif = format_verification_list(&plan.verification_steps);
    let steps_human = plan
        .steps
        .iter()
        .enumerate()
        .map(|(i, st)| format_step_human_summary(st, i))
        .collect::<Vec<_>>()
        .join("\n");
    let steps_json = format_plan_steps_json_block(&plan.steps);
    format!(
        "# Plan\n\n**Goal:** {}\n\n**Success Criteria:** {}\n\n## Verification\n{}\n\n**Rollback:** {}\n\n**Constraints:** {}\n\n## Steps\n{}\n{}\n## Execution Log\n\n(Added as work proceeds after 'proceed'.)\n",
        plan.goal,
        plan.success_criteria,
        verif,
        plan.rollback,
        plan.constraints,
        steps_human,
        steps_json
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan_state::PlanStep;

    #[test]
    fn execution_user_prompt_lists_steps_and_wiki_hint() {
        let mut plan = PlanState::default();
        plan.goal = "Build game".into();
        plan.success_criteria = "Runs".into();
        plan.current_step = 0;
        plan.steps = vec![PlanStep {
            description: "mkdir galaga".into(),
            verification: Some("mkdir -p galaga".into()),
            tier: Some(PlanStepTier::Exec),
            status: PlanStepStatus::InProgress,
            ..Default::default()
        }];
        let ws = std::env::temp_dir();
        let prompt = format_plan_execution_user_prompt(&plan, &ws);
        assert!(prompt.contains("mkdir galaga"));
        assert!(prompt.contains("wiki plan.md"));
        assert!(prompt.contains("← CURRENT"));
        assert!(prompt.contains("Begin with step 1"));
        assert!(prompt.contains("Deliverable location"));
        assert!(prompt.contains("galaga/"));
    }

    #[test]
    fn format_plan_steps_json_block_roundtrip() {
        let steps = vec![PlanStep {
            description: "step a".into(),
            verification: Some("true".into()),
            tier: Some(PlanStepTier::Exec),
            note: Some("note".into()),
            ..Default::default()
        }];
        let block = format_plan_steps_json_block(&steps);
        assert!(block.contains("plan-steps:json"));
        let json = plan_md::extract_plan_steps_json(&block).expect("json");
        let parsed = plan_md::parse_plan_steps_json(&json).expect("parse");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].description, "step a");
    }

    #[test]
    fn format_plan_status_shows_goal_and_steps() {
        let mut plan = PlanState {
            goal: "Ship it".into(),
            success_criteria: "Tests pass".into(),
            steps: vec![PlanStep {
                description: "run tests".into(),
                status: PlanStepStatus::Done,
                tier: Some(PlanStepTier::Exec),
                ..Default::default()
            }],
            current_step: 1,
            ..Default::default()
        };
        let text = format_plan_status(&plan, "work");
        assert!(text.contains("Ship it"));
        assert!(text.contains("run tests"));
        plan.complete();
        let done = format_plan_status(&plan, "work");
        assert!(done.contains("1/1 complete"));
    }
}