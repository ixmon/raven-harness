//! Plan progress sync between TUI `PlanState`, agent `PlanExecutionState`, and wiki log.

use crate::plan_state::{PlanState, PlanStep, PlanStepStatus, PlanStepTier};
use raven_tui::agent::Agent;
use raven_tui::plan_execution;
use raven_tui::plan_md::{self, PlanExecutionState, PlanStepData};
use std::path::Path;

pub fn plan_step_to_data(step: &PlanStep) -> PlanStepData {
    PlanStepData {
        description: step.description.clone(),
        verification: step.verification.clone(),
        tier: step.tier.map(|t| t.as_str().to_string()),
        note: step.note.clone(),
        observe_prompt: step.observe_prompt.clone(),
        status: match step.status {
            PlanStepStatus::Done => "done".to_string(),
            PlanStepStatus::Failed => "failed".to_string(),
            PlanStepStatus::InProgress => "in_progress".to_string(),
            PlanStepStatus::Pending => "pending".to_string(),
        },
    }
}

fn apply_data_to_plan_step(step: &mut PlanStep, data: &PlanStepData) {
    step.status = match data.status.as_str() {
        "done" => PlanStepStatus::Done,
        "failed" => PlanStepStatus::Failed,
        "in_progress" => PlanStepStatus::InProgress,
        _ => PlanStepStatus::Pending,
    };
    step.tier = data.tier.as_deref().and_then(PlanStepTier::parse);
}

pub fn plan_state_to_execution(plan: &PlanState, workspace: &Path) -> PlanExecutionState {
    let steps: Vec<PlanStepData> = plan.steps.iter().map(plan_step_to_data).collect();
    let project_workdir = plan
        .project_workdir
        .clone()
        .or_else(|| plan_execution::detect_project_workdir(workspace, &steps));
    PlanExecutionState {
        active: plan.active && !plan.steps.is_empty(),
        steps,
        current_step: plan.current_step,
        project_workdir,
        pending_observe_prompt: plan.pending_observe_prompt.clone(),
        pending_observe_step: plan.pending_observe_step,
    }
}

pub fn apply_execution_to_plan(plan: &mut PlanState, exec: &PlanExecutionState) {
    plan.current_step = exec.current_step;
    plan.pending_observe_prompt = exec.pending_observe_prompt.clone();
    plan.pending_observe_step = exec.pending_observe_step;
    for (i, data) in exec.steps.iter().enumerate() {
        if let Some(step) = plan.steps.get_mut(i) {
            apply_data_to_plan_step(step, data);
        }
    }
}

/// Apply durable PASS lines from `wiki/plan.md` onto in-memory plan progress.
pub fn apply_execution_log_to_plan(plan: &mut PlanState, wiki_content: &str) {
    if plan.steps.is_empty() {
        return;
    }
    let passed = plan_md::parse_execution_log_passed_steps(wiki_content);
    if passed.is_empty() {
        return;
    }
    for &n in &passed {
        if let Some(step) = plan.steps.get_mut(n - 1) {
            step.status = PlanStepStatus::Done;
        }
    }
    let highest = *passed.iter().max().unwrap();
    let new_current = highest.min(plan.steps.len());
    if new_current > plan.current_step {
        plan.current_step = new_current;
    }
    if plan.current_step < plan.steps.len()
        && plan.steps[plan.current_step].status != PlanStepStatus::Done
    {
        plan.steps[plan.current_step].status = PlanStepStatus::InProgress;
    }
}

/// Merge wiki execution log + agent execution state into the TUI plan pane (never regress).
pub fn reconcile_plan_execution(
    plan: &mut PlanState,
    agent: &Agent,
    wiki_content: Option<&str>,
) {
    if !plan.active || plan.steps.is_empty() {
        return;
    }
    if let Some(wiki) = wiki_content {
        apply_execution_log_to_plan(plan, wiki);
    }
    let exec = agent.plan_execution();
    if exec.active
        && !exec.steps.is_empty()
        && exec.steps.len() == plan.steps.len()
        && exec.current_step > plan.current_step
    {
        apply_execution_to_plan(plan, exec);
    } else if exec.active
        && !exec.steps.is_empty()
        && exec.steps.len() == plan.steps.len()
    {
        for (i, data) in exec.steps.iter().enumerate() {
            if let Some(step) = plan.steps.get_mut(i) {
                if data.status == "done" && step.status != PlanStepStatus::Done {
                    step.status = PlanStepStatus::Done;
                }
            }
        }
    }
}

pub fn sync_plan_to_agent(agent: &mut Agent, plan: &PlanState, workspace: &Path) {
    let incoming = plan_state_to_execution(plan, workspace);
    let existing = agent.plan_execution();
    if incoming.active
        && !incoming.steps.is_empty()
        && existing.active
        && !existing.steps.is_empty()
        && existing.steps.len() == incoming.steps.len()
        && existing.current_step > incoming.current_step
    {
        let mut merged = existing.clone();
        if merged.project_workdir.is_none() {
            merged.project_workdir = incoming.project_workdir;
        }
        for (i, step) in merged.steps.iter_mut().enumerate() {
            if let Some(in_step) = incoming.steps.get(i) {
                if in_step.status == "done" {
                    step.status = "done".to_string();
                }
            }
        }
        merged.current_step = merged.current_step.max(incoming.current_step);
        if merged.current_step < merged.steps.len()
            && merged.steps[merged.current_step].status != "done"
        {
            merged.steps[merged.current_step].status = "in_progress".to_string();
        }
        agent.set_plan_execution(merged);
        return;
    }
    agent.set_plan_execution(incoming);
}

pub fn sync_plan_from_agent(plan: &mut PlanState, agent: &Agent) {
    apply_execution_to_plan(plan, agent.plan_execution());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan_state::PlanStep;
    use raven_tui::agent::Agent;
    use raven_tui::chat_backend::{ChatBackend, MockChatBackend};
    use raven_tui::config::{Config, ContextBudget, ContextSource};
    use raven_tui::tools::backend::{MockToolBackend, ToolBackend};

    #[test]
    fn execution_log_hydrates_stale_plan_pane() {
        let mut plan = PlanState {
            active: true,
            steps: (1..=8)
                .map(|n| PlanStep {
                    description: format!("step {n}"),
                    status: PlanStepStatus::Pending,
                    ..Default::default()
                })
                .collect(),
            current_step: 3,
            ..Default::default()
        };
        let wiki = r#"## Execution Log
- Step 1 exec `true` → PASS
- Step 2 exec `true` → PASS
- Step 3 exec `true` → PASS
- Step 4 exec `true` → PASS
- Step 5 exec `true` → PASS
- Step 6 exec `true` → PASS
- Step 7 exec `true` → PASS
- Step 8 exec `true` → PASS
"#;
        apply_execution_log_to_plan(&mut plan, wiki);
        assert_eq!(plan.current_step, 8);
        assert!(plan.steps.iter().all(|s| s.status == PlanStepStatus::Done));
    }

    #[test]
    fn sync_plan_to_agent_does_not_regress_progress() {
        let workspace =
            std::env::temp_dir().join(format!("raven_plan_sync_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&workspace);
        let cfg = Config {
            base_url: "http://mock.local/v1".into(),
            model: "mock".into(),
            api_key: None,
            workspace,
            temperature: 0.0,
            max_tokens: 512,
            max_rounds: 5,
            prebuilt_session: None,
            context_budget: ContextBudget {
                context_tokens: 8192,
                tool_result_bytes: 4000,
                read_line_limit: 80,
                source: ContextSource::Default,
            },
            tool_backend: ToolBackend::Mock(MockToolBackend::default()),
            tools_enabled: true,
            enable_judge: false,
            flags: raven_tui::runtime::RuntimeFlags::default(),
            harness: raven_tui::runtime::EvalHarness::default(),
            openrouter_reasoning: raven_tui::config::OpenRouterReasoningMode::Auto,
        };
        let chat = ChatBackend::Mock(MockChatBackend::new(vec![]));
        let mut ag = Agent::new(cfg, chat);
        ag.set_plan_execution(PlanExecutionState {
            active: true,
            steps: vec![
                PlanStepData {
                    description: "a".into(),
                    status: "done".into(),
                    ..Default::default()
                },
                PlanStepData {
                    description: "b".into(),
                    status: "in_progress".into(),
                    ..Default::default()
                },
            ],
            current_step: 1,
            ..Default::default()
        });
        let stale_plan = PlanState {
            active: true,
            steps: vec![
                PlanStep {
                    description: "a".into(),
                    status: PlanStepStatus::Done,
                    ..Default::default()
                },
                PlanStep {
                    description: "b".into(),
                    status: PlanStepStatus::Pending,
                    ..Default::default()
                },
            ],
            current_step: 0,
            ..Default::default()
        };
        let workspace = ag.workspace().to_path_buf();
        sync_plan_to_agent(&mut ag, &stale_plan, &workspace);
        assert_eq!(ag.plan_execution().current_step, 1);
    }
}