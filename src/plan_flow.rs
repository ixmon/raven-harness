//! Plan mode entry, proceed confirmation, and plan.md parsing.

use crate::app_state::{App, Pane};
use crate::plan_state::{PlanLoopPhase, PlanState, PlanStep, PlanStepStatus, PlanStepTier};
use raven_tui::chat_backend::ChatBackend;
use raven_tui::plan_intent::{
    classify_plan_answer, classify_plan_entry_intent, classify_proposal_consent,
    is_explicit_proceed_approval, PlanAnswerResolution, PlanEntryIntent, PlanProceedIntent,
};
use raven_tui::plan_loop::{fetch_clarification, fetch_proposal};
use raven_tui::plan_verification::{improve_proposal, resolve_project_workdir};
use raven_tui::plan_protocol::{
    format_proposal_for_user, format_question_for_user, PlanModelPayload, PlanProposal,
    PlanProposalStep, PlanQaEntry,
};
use raven_tui::runtime::RuntimeFlags;
use raven_tui::agent::Agent;
use raven_tui::plan_md::{self, ParsedPlanStep, PLAN_STEPS_JSON_MARKER};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::sync::Mutex as TokioMutex;
use tokio::sync::Mutex;

use crate::event_loop::UiUpdate;

/// Which context is requesting default verification commands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VerificationDefaultsKind {
    /// User confirmed plan mode entry (`y` on trigger dialog).
    PlanEntry,
    /// User said proceed but verification list is still empty.
    ProceedFallback,
    /// `event_loop` auto-activation while `agent_mode == "plan"`.
    AutoActivate,
}

/// Parsed content from `wiki/plan.md`.
#[derive(Debug, Default)]
pub struct ParsedPlanMd {
    pub steps: Vec<ParsedPlanStep>,
    pub verification: Vec<String>,
    /// True when steps came from the `plan-steps:json` comment block.
    pub structured: bool,
}

/// Result of handling plan-related input before a normal agent submit.
#[derive(Debug, PartialEq, Eq)]
pub enum PlanSubmitOutcome {
    /// Plan flow consumed the submit; do not continue.
    Stop,
    /// Legacy variant (unused — JSON loop uses StartPlanLoop).
    #[allow(dead_code)]
    Continue(String),
    /// Entered plan mode — start JSON clarify loop (do not drive_turn yet).
    StartPlanLoop,
}

/// Outcome of a user message during the JSON plan loop.
#[derive(Debug, PartialEq, Eq)]
pub enum PlanLoopUserOutcome {
    Consumed,
    /// User approved proposal — switch to work and run execution turn.
    StartExecution,
    /// Classify answer + fetch next clarification in background.
    SpawnAnswer {
        user_input: String,
        question: raven_tui::plan_protocol::PlanQuestion,
    },
    /// Classify proceed feedback in background.
    SpawnProceedFeedback {
        user_input: String,
    },
}

/// Outcome of cheap LLM/heuristic plan intent routing on a user submit.
#[derive(Debug, PartialEq, Eq)]
pub enum PlanInputRouting {
    /// No plan-specific handling applied.
    Pass,
    /// Plan flow handled side effects; still submit the prompt to the agent.
    Continue,
    /// Plan flow consumed the submit (dialog opened, cancel, etc.).
    Stop,
}

/// Returns true if the prompt looks like a request to enter plan mode.
pub(crate) fn is_plan_trigger_phrase(prompt: &str) -> bool {
    let lower = prompt.to_lowercase();
    let trigger_phrases = [
        "come up with a plan",
        "let's plan",
        "make a plan",
        "first plan",
        "what's the plan",
        "plan the",
        "create a plan",
        "develop a plan",
        "plan out",
        "plan for this",
    ];
    trigger_phrases.iter().any(|p| lower.contains(p))
        || (lower.contains("plan")
            && (lower.contains("task")
                || lower.contains("work")
                || lower.contains("refactor")
                || lower.contains("change")
                || lower.contains("implement")))
}

/// Returns true for phrases that should confirm "proceed" while in plan mode.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn is_proceed_confirmation(prompt: &str) -> bool {
    let lower = prompt.to_lowercase();
    lower.contains("proceed")
        || lower.contains("go ahead")
        || lower.contains("let's go")
        || lower == "yes"
        || lower.contains("start executing")
        || lower.contains("let's do it")
        || lower.contains("do it")
        || lower.contains("go for it")
        || lower.contains("confirmed")
}

/// Task-aware default verification commands for a goal string.
pub fn derive_verification_defaults(goal: &str, kind: VerificationDefaultsKind) -> Vec<String> {
    let g = goal.to_lowercase();
    if g.contains("python") || g.contains(".py") {
        return match kind {
            VerificationDefaultsKind::ProceedFallback => vec![
                "python3 birthday_cake.py".to_string(),
                "output contains recognizable ASCII cake".to_string(),
            ],
            VerificationDefaultsKind::PlanEntry => vec![
                "python3 <your_script>.py [args]".to_string(),
                "check that output matches the expected result".to_string(),
            ],
            VerificationDefaultsKind::AutoActivate => vec![
                "python3 <script>.py".to_string(),
                "check script output".to_string(),
            ],
        };
    }
    if g.contains("c++") || g.contains("cpp") || g.contains("g++") || g.contains("clang") {
        return vec![
            "g++ -std=c++17 -Wall -o program program.cpp".to_string(),
            "clang-tidy program.cpp -- -std=c++17".to_string(),
            "./program".to_string(),
        ];
    }
    if kind == VerificationDefaultsKind::PlanEntry && (g.contains("c ") || g.contains("gcc")) {
        return vec![
            "gcc -Wall -o program program.c".to_string(),
            "./program".to_string(),
        ];
    }
    vec![
        "cargo check".to_string(),
        "cargo clippy -- -D warnings".to_string(),
        "cargo test".to_string(),
    ]
}

/// Clear plan fields when starting a fresh planning request.
pub fn reset_plan_for_new_request(plan: &mut PlanState) {
    plan.success_criteria.clear();
    plan.verification_steps.clear();
    plan.rollback.clear();
    plan.constraints.clear();
    plan.project_workdir = None;
    plan.steps.clear();
    plan.current_step = 0;
    plan.recap_offered = false;
    plan.loop_phase = PlanLoopPhase::Idle;
    plan.initial_request.clear();
    plan.qa_history.clear();
    plan.pending_question = None;
    plan.pending_proposal = None;
}

fn start_json_plan_loop(plan: &mut PlanState, initial_request: &str) {
    plan.initial_request = initial_request.to_string();
    plan.goal = initial_request.to_string();
    plan.qa_history.clear();
    plan.pending_question = None;
    plan.pending_proposal = None;
    plan.recap_offered = false;
    plan.steps.clear();
    plan.loop_phase = PlanLoopPhase::FetchingQuestion;
}

/// True when assistant text invited the user to approve the plan for execution.
pub fn detect_plan_recap_invite(text: &str) -> bool {
    let lower = text.to_lowercase();
    [
        "shall we proceed",
        "ready to proceed",
        "proceed with this plan",
        "want to proceed",
        "should we proceed",
        "approve this plan",
        "start executing this plan",
    ]
    .iter()
    .any(|p| lower.contains(p))
}

/// Fill empty plan metadata fields (does not set goal or steps).
pub fn apply_plan_field_defaults(plan: &mut PlanState, kind: VerificationDefaultsKind) {
    if plan.success_criteria.is_empty() {
        plan.success_criteria = match kind {
            VerificationDefaultsKind::AutoActivate => {
                "Verification passes and goal achieved".to_string()
            }
            _ => "Verification steps pass and the goal is achieved".to_string(),
        };
    }
    if plan.verification_steps.is_empty() && !plan.goal.is_empty() {
        plan.verification_steps = derive_verification_defaults(&plan.goal, kind);
    }
    if plan.rollback.is_empty() {
        plan.rollback = "git branch + checkpoints".to_string();
    }
    if plan.constraints.is_empty() && kind == VerificationDefaultsKind::AutoActivate {
        plan.constraints = "Stay on feature branch; keep changes reviewable".to_string();
    }
}

pub fn extract_plan_steps_json(content: &str) -> Option<String> {
    plan_md::extract_plan_steps_json(content)
}

pub fn parse_plan_steps_json(json: &str) -> Option<Vec<ParsedPlanStep>> {
    plan_md::parse_plan_steps_json(json)
}

use crate::plan_prompts::{format_plan_execution_user_prompt, format_plan_status};

fn parsed_step_to_plan_step(step: ParsedPlanStep) -> PlanStep {
    let tier = step.tier.as_deref().and_then(PlanStepTier::parse);
    let (verification, observe_prompt) = if tier == Some(PlanStepTier::Observe) {
        (
            None,
            step.prompt
                .or(step.verification)
                .filter(|s| !s.trim().is_empty()),
        )
    } else {
        (
            step.verification
                .filter(|s| !s.trim().is_empty()),
            step.prompt.filter(|s| !s.trim().is_empty()),
        )
    };
    PlanStep {
        description: step.description,
        verification,
        tier,
        note: step.note.filter(|s| !s.trim().is_empty()),
        observe_prompt,
        status: PlanStepStatus::Pending,
    }
}

/// Extract steps and verification lists from agent-written plan markdown.
pub fn parse_plan_md(content: &str) -> ParsedPlanMd {
    let mut verification = parse_verification_heuristic(content);

    if let Some(json) = extract_plan_steps_json(content) {
        if let Some(steps) = parse_plan_steps_json(&json) {
            if verification.is_empty() {
                verification = steps
                    .iter()
                    .filter_map(|s| s.verification.clone())
                    .collect();
            }
            return ParsedPlanMd {
                steps,
                verification,
                structured: true,
            };
        }
    }

    ParsedPlanMd {
        steps: parse_steps_heuristic(content),
        verification,
        structured: false,
    }
}

fn parse_verification_heuristic(content: &str) -> Vec<String> {
    let mut extracted_verif = Vec::new();
    let mut in_verif = false;
    for line in content.lines() {
        let t = line.trim();
        if t.eq_ignore_ascii_case("**verification:**")
            || t.eq_ignore_ascii_case("verification:")
            || t.to_lowercase().starts_with("**verification")
            || t.eq_ignore_ascii_case("## verification")
        {
            in_verif = true;
            continue;
        }
        if t.eq_ignore_ascii_case("**steps:**")
            || t.to_lowercase().starts_with("**steps")
            || t.eq_ignore_ascii_case("steps:")
            || t.eq_ignore_ascii_case("## steps")
            || t.starts_with(PLAN_STEPS_JSON_MARKER)
        {
            in_verif = false;
            if t.starts_with(PLAN_STEPS_JSON_MARKER) {
                break;
            }
            continue;
        }
        if in_verif {
            if t.starts_with('#')
                || t.starts_with("**") && t.contains("step")
                || t.to_lowercase().starts_with("rollback")
            {
                in_verif = false;
            } else if t.starts_with("- ") || t.starts_with("* ") || t.starts_with("1. ") {
                let v = t
                    .trim_start_matches(|c: char| {
                        c == '-' || c == '*' || c == ' ' || c.is_ascii_digit() || c == '.'
                    })
                    .trim();
                if !v.is_empty() {
                    extracted_verif.push(v.to_string());
                }
            }
        }
    }
    extracted_verif
}

fn parse_steps_heuristic(content: &str) -> Vec<ParsedPlanStep> {
    let mut extracted_steps = Vec::new();
    let mut in_steps = false;
    for line in content.lines() {
        let t = line.trim();
        if t.starts_with(PLAN_STEPS_JSON_MARKER) {
            break;
        }
        if t.eq_ignore_ascii_case("**steps:**")
            || t.to_lowercase().starts_with("**steps")
            || t.eq_ignore_ascii_case("steps:")
            || t.eq_ignore_ascii_case("## steps")
        {
            in_steps = true;
            continue;
        }
        if !in_steps {
            continue;
        }
        if t.starts_with('#') || t.to_lowercase().starts_with("rollback") {
            break;
        }
        if let Some(rest) = t
            .strip_prefix(|c: char| c.is_ascii_digit())
            .and_then(|r| r.strip_prefix('.').or_else(|| r.strip_prefix(")")))
        {
            let desc = rest.trim_start_matches([' ', '-', '*']).trim();
            if !desc.is_empty()
                && desc.len() > 3
                && !desc.to_lowercase().starts_with("verify")
            {
                extracted_steps.push(ParsedPlanStep {
                    description: desc.to_string(),
                    tier: None,
                    verification: None,
                    prompt: None,
                    note: None,
                });
            }
        } else if t.starts_with("- ") || t.starts_with("* ") {
            let desc = t[2..].trim();
            if !desc.is_empty()
                && desc.len() > 5
                && !desc.to_lowercase().contains("rollback")
                && !desc.to_lowercase().contains("constraint")
                && !desc.to_lowercase().contains("verify")
            {
                extracted_steps.push(ParsedPlanStep {
                    description: desc.to_string(),
                    tier: None,
                    verification: None,
                    prompt: None,
                    note: None,
                });
            }
        }
    }
    extracted_steps
}

/// Build fallback execution steps when plan.md has no extractable step list.
pub fn derive_fallback_steps(goal: &str, verif: &[String]) -> Vec<PlanStep> {
    let g = goal.to_lowercase();
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
        PlanStep {
            description: step1,
            verification: verif.first().cloned(),
            tier: Some(PlanStepTier::Exec),
            note: None,
            observe_prompt: None,
            status: PlanStepStatus::Pending,
        },
        PlanStep {
            description: step2,
            verification: verif.get(1).cloned(),
            tier: Some(PlanStepTier::Exec),
            note: None,
            observe_prompt: None,
            status: PlanStepStatus::Pending,
        },
        PlanStep {
            description: step3,
            verification: verif.get(verif.len().saturating_sub(1)).cloned(),
            tier: Some(PlanStepTier::Exec),
            note: None,
            observe_prompt: None,
            status: PlanStepStatus::Pending,
        },
    ]
}

/// Handle y/n response for pending plan mode entry. Returns input to re-submit if confirmed.
pub fn handle_plan_entry_confirmation(
    app: &mut App,
    agent: &Arc<TokioMutex<Agent>>,
    submitted: &str,
    original_request: Option<String>,
) -> PlanSubmitOutcome {
    let yes = submitted.trim().to_lowercase().starts_with('y');
    if !yes {
        app.left_committed
            .push("Plan mode entry cancelled.".to_string());
        app.needs_redraw = true;
        return PlanSubmitOutcome::Stop;
    }

    if let Ok(mut ag) = agent.try_lock() {
        ag.set_agent_mode("plan");
        if let Some(s) = &mut ag.session_mut() {
            let _ = s.save_meta();
        }
    }
    app.plan.active = true;
    app.focused_pane = Pane::Input;

    if let Some(ref req) = original_request {
        app.plan.goal = req.clone();
        if let Ok(mut ag) = agent.try_lock() {
            if let Some(s) = &mut ag.session_mut() {
                s.meta.current_goal = req.clone();
                let _ = s.save_meta();
            }
        }
    } else if app.plan.goal.is_empty() {
        app.plan.goal = "Plan for the current task".to_string();
    }

    apply_plan_field_defaults(&mut app.plan, VerificationDefaultsKind::PlanEntry);

    if let Ok(mut ag) = agent.try_lock() {
        if let Some(s) = ag.session_mut() {
            let _ = s.write_wiki_file("plan.md", &crate::plan_prompts::wiki_template_on_entry(&app.plan));
            app.left_committed.push(
                "Plan written to session wiki/plan.md (you can edit externally too)".to_string(),
            );
        }
    }

    app.plan.steps.clear();
    app.plan.current_step = 0;

    let initial = original_request
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| app.plan.goal.clone());

    start_json_plan_loop(&mut app.plan, &initial);
    app.left_committed.push(
        "Entered Plan Mode (JSON loop). Gathering clarifications…".to_string(),
    );

    PlanSubmitOutcome::StartPlanLoop
}

fn proposal_step_to_plan_step(st: &PlanProposalStep) -> PlanStep {
    let tier = st.tier.as_deref().and_then(PlanStepTier::parse);
    let (verification, observe_prompt) = if tier == Some(PlanStepTier::Observe) {
        (
            None,
            st.prompt
                .as_deref()
                .or(st.verification.as_deref())
                .map(|s| s.to_string()),
        )
    } else {
        (st.verification.clone(), st.prompt.clone())
    };
    PlanStep {
        description: st.description.clone(),
        verification,
        tier,
        note: st.note.clone(),
        observe_prompt,
        status: PlanStepStatus::Pending,
    }
}

pub fn apply_proposal_to_plan(plan: &mut PlanState, proposal: &PlanProposal) {
    plan.goal = proposal.goal.clone();
    plan.success_criteria = proposal.success_criteria.clone();
    plan.verification_steps = proposal.verification.clone();
    plan.rollback = proposal.rollback.clone().unwrap_or_default();
    plan.constraints = proposal.constraints.clone().unwrap_or_default();
    plan.steps = proposal
        .steps
        .iter()
        .map(proposal_step_to_plan_step)
        .collect();
    if !plan.steps.is_empty() {
        plan.steps[0].status = PlanStepStatus::InProgress;
    }
    plan.current_step = 0;
}

fn resolution_label(res: &PlanAnswerResolution) -> String {
    match res {
        PlanAnswerResolution::Selected { label, .. } => format!("Selected: {label}"),
        PlanAnswerResolution::FreeText(t) => format!("Free text: {t}"),
        PlanAnswerResolution::DeferToRecommend => "Deferred to recommended option".to_string(),
        PlanAnswerResolution::ExitDiscuss => "Exit discuss".to_string(),
        PlanAnswerResolution::ReviseProposal => "Revise proposal".to_string(),
    }
}

/// Process clarify/proposal JSON from the model.
pub fn handle_plan_loop_model_payload(
    app: &mut App,
    agent: &Arc<TokioMutex<Agent>>,
    payload: PlanModelPayload,
) {
    match payload {
        PlanModelPayload::Clarify { question } => {
            app.plan.pending_question = Some(question.clone());
            app.plan.loop_phase = PlanLoopPhase::AwaitingUserAnswer;
            app.left_committed
                .push(format_question_for_user(&question));
            app.needs_redraw = true;
        }
        PlanModelPayload::Ready { message } => {
            if let Some(m) = message {
                app.left_committed.push(m);
            }
            app.plan.loop_phase = PlanLoopPhase::FetchingProposal;
            app.needs_redraw = true;
        }
        PlanModelPayload::Proposal(proposal) => {
            present_proposal(app, agent, proposal);
        }
    }
}

fn present_proposal(app: &mut App, agent: &Arc<TokioMutex<Agent>>, mut proposal: PlanProposal) {
    let workdir = resolve_project_workdir(
        &app.plan.initial_request,
        &app.plan.qa_history,
        &proposal.steps,
    );
    app.plan.project_workdir = workdir.clone();
    let report = improve_proposal(&mut proposal, workdir.as_deref());
    apply_proposal_to_plan(&mut app.plan, &proposal);
    app.plan.pending_proposal = Some(proposal.clone());
    app.plan.recap_offered = true;
    app.plan.loop_phase = PlanLoopPhase::AwaitingProceedConsent;
    let mut recap = format_proposal_for_user(&proposal);
    if let Some(wd) = &app.plan.project_workdir {
        recap.push_str(&format!("\n\n📁 **Project directory:** `{wd}/` (all deliverables here)\n"));
    }
    if !report.fixes.is_empty() {
        recap.push_str("\n\n⚙ Harness adjusted verifications:\n");
        for fix in &report.fixes {
            recap.push_str(&format!("  • {fix}\n"));
        }
    }
    if !report.errors.is_empty() {
        recap.push_str("\n\n⚠ Remaining verification issues (please revise before proceed):\n");
        for err in &report.errors {
            recap.push_str(&format!("  • {err}\n"));
        }
    }
    if !report.warnings.is_empty() {
        recap.push_str("\n\n💡 Verification advisories (non-blocking, review before proceed):\n");
        for w in &report.warnings {
            recap.push_str(&format!("  • {w}\n"));
        }
    }
    app.left_committed.push(recap);

    if let Ok(mut ag) = agent.try_lock() {
        if let Some(s) = ag.session_mut() {
            let _ = s.write_wiki_file("plan.md", &crate::plan_prompts::wiki_template_approved(&app.plan));
            let _ = s.update_goal(
                &app.plan.goal,
                Some(app.plan.verification_steps.clone()),
                None,
            );
        }
    }
    app.needs_redraw = true;
}

/// Apply clarify fetch result on the UI thread.
pub fn apply_plan_clarify_done(
    app: &mut App,
    agent: &Arc<TokioMutex<Agent>>,
    result: Result<PlanModelPayload, String>,
    qa_entry: Option<PlanQaEntry>,
) {
    if let Some(entry) = qa_entry {
        app.plan.qa_history.push(entry);
    }
    match result {
        Ok(payload) => handle_plan_loop_model_payload(app, agent, payload),
        Err(e) => {
            app.plan.loop_phase = PlanLoopPhase::Idle;
            app.left_committed
                .push(format!("Plan clarify failed: {e}"));
            app.needs_redraw = true;
        }
    }
}

/// Apply proposal fetch result on the UI thread.
pub fn apply_plan_proposal_done(
    app: &mut App,
    agent: &Arc<TokioMutex<Agent>>,
    result: Result<PlanModelPayload, String>,
) {
    match result {
        Ok(PlanModelPayload::Proposal(proposal)) => {
            present_proposal(app, agent, proposal);
        }
        Ok(other) => {
            app.left_committed.push(format!(
                "Plan proposal returned unexpected type: {other:?}"
            ));
            app.plan.loop_phase = PlanLoopPhase::Idle;
            app.needs_redraw = true;
        }
        Err(e) => {
            app.plan.loop_phase = PlanLoopPhase::Idle;
            app.left_committed
                .push(format!("Plan proposal failed: {e}"));
            app.needs_redraw = true;
        }
    }
}

pub fn apply_plan_exit_discuss(app: &mut App, agent: &Arc<TokioMutex<Agent>>) {
    app.plan.loop_phase = PlanLoopPhase::Idle;
    app.plan.pending_question = None;
    if let Ok(mut ag) = agent.try_lock() {
        ag.set_agent_mode("talk");
        if let Some(s) = ag.session_mut() {
            let _ = s.save_meta();
        }
    }
    app.left_committed
        .push("Exited structured plan loop — free discussion.".to_string());
    app.needs_redraw = true;
}

fn mark_plan_fetching(app: &mut App, phase: PlanLoopPhase, trace: &str) {
    app.plan.loop_phase = phase;
    app.current_response = "⠋ Planning…\n".to_string();
    app.left_follow_output = true;
    app.left_scroll = 10_000;
    app.trace_lines.push(trace.to_string());
    app.right_follow_output = true;
    app.right_scroll = 10_000;
    app.needs_redraw = true;
}

async fn clarify_then_proposal(
    backend: &Arc<Mutex<ChatBackend>>,
    flags: &RuntimeFlags,
    initial: &str,
    history: &[PlanQaEntry],
    workspace: &str,
    tx: &mpsc::Sender<UiUpdate>,
    qa_entry: Option<PlanQaEntry>,
) {
    let clarify = fetch_clarification(backend, flags, initial, history, workspace).await;
    if let Ok(PlanModelPayload::Ready { message }) = &clarify {
        if let Some(m) = message {
            let _ = tx.send(UiUpdate::PlanLoopStatusMessage(m.clone())).await;
        }
        let proposal = fetch_proposal(backend, flags, initial, history, workspace).await;
        let _ = tx.send(UiUpdate::PlanLoopProposalDone(proposal)).await;
        return;
    }
    let _ = tx
        .send(UiUpdate::PlanLoopClarifyDone {
            result: clarify,
            qa_entry,
        })
        .await;
}

/// Start initial or follow-up clarify fetch without blocking the TUI event loop.
pub fn spawn_fetch_clarification(
    app: &mut App,
    backend: Arc<Mutex<ChatBackend>>,
    flags: RuntimeFlags,
    workspace: String,
    tx: mpsc::Sender<UiUpdate>,
) {
    mark_plan_fetching(
        app,
        PlanLoopPhase::FetchingQuestion,
        "📋 Plan: fetching clarification…",
    );
    let initial = app.plan.initial_request.clone();
    let history = app.plan.qa_history.clone();
    tokio::spawn(async move {
        clarify_then_proposal(&backend, &flags, &initial, &history, &workspace, &tx, None).await;
    });
}

pub fn spawn_plan_answer_submit(
    app: &mut App,
    backend: Arc<Mutex<ChatBackend>>,
    flags: RuntimeFlags,
    workspace: String,
    tx: mpsc::Sender<UiUpdate>,
    user_input: String,
    question: raven_tui::plan_protocol::PlanQuestion,
) {
    let job = PlanAnswerJob {
        user_input,
        question,
        initial_request: app.plan.initial_request.clone(),
        qa_history: app.plan.qa_history.clone(),
    };
    spawn_plan_answer_work(app, backend, flags, workspace, tx, job);
}

struct PlanAnswerJob {
    user_input: String,
    question: raven_tui::plan_protocol::PlanQuestion,
    initial_request: String,
    qa_history: Vec<PlanQaEntry>,
}

fn spawn_plan_answer_work(
    app: &mut App,
    backend: Arc<Mutex<ChatBackend>>,
    flags: RuntimeFlags,
    workspace: String,
    tx: mpsc::Sender<UiUpdate>,
    job: PlanAnswerJob,
) {
    mark_plan_fetching(
        app,
        PlanLoopPhase::FetchingQuestion,
        "📋 Plan: classifying answer…",
    );
    tokio::spawn(async move {
        let resolution =
            classify_plan_answer(&backend, &flags, &job.user_input, &job.question).await;
        if matches!(resolution, PlanAnswerResolution::ExitDiscuss) {
            let _ = tx.send(UiUpdate::PlanLoopExitDiscuss).await;
            return;
        }
        let label = resolution_label(&resolution);
        let qa_entry = PlanQaEntry {
            question_id: job.question.id.clone(),
            question_prompt: job.question.prompt.clone(),
            user_input: job.user_input.clone(),
            resolution: label,
        };
        let mut history = job.qa_history;
        history.push(qa_entry.clone());
        clarify_then_proposal(
            &backend,
            &flags,
            &job.initial_request,
            &history,
            &workspace,
            &tx,
            Some(qa_entry),
        )
        .await;
    });
}

/// Synchronous plan-loop submit: updates UI immediately and returns whether to spawn background work.
pub fn submit_plan_loop_input(
    app: &mut App,
    _agent: &Arc<TokioMutex<Agent>>,
    user_input: &str,
) -> PlanLoopUserOutcome {
    match app.plan.loop_phase {
        PlanLoopPhase::AwaitingUserAnswer => {
            let question = match app.plan.pending_question.take() {
                Some(q) => q,
                None => return PlanLoopUserOutcome::Consumed,
            };
            app.left_committed.push(format!("> {}", user_input));
            app.needs_redraw = true;
            PlanLoopUserOutcome::SpawnAnswer {
                user_input: user_input.to_string(),
                question,
            }
        }
        PlanLoopPhase::AwaitingProceedConsent => {
            app.left_committed.push(format!("> {}", user_input));
            if is_explicit_proceed_approval(user_input) {
                app.needs_redraw = true;
                return PlanLoopUserOutcome::StartExecution;
            }
            app.needs_redraw = true;
            PlanLoopUserOutcome::SpawnProceedFeedback {
                user_input: user_input.to_string(),
            }
        }
        _ => PlanLoopUserOutcome::Consumed,
    }
}

pub fn spawn_proceed_feedback_work(
    app: &mut App,
    backend: Arc<Mutex<ChatBackend>>,
    flags: RuntimeFlags,
    workspace: String,
    tx: mpsc::Sender<UiUpdate>,
    user_input: String,
) {
    mark_plan_fetching(
        app,
        PlanLoopPhase::FetchingProposal,
        "📋 Plan: interpreting feedback…",
    );
    app.plan.pending_proposal = None;
    app.plan.recap_offered = false;
    let initial = app.plan.initial_request.clone();
    let base_history = app.plan.qa_history.clone();
    tokio::spawn(async move {
        let intent = classify_proposal_consent(&backend, &flags, &user_input).await;
        match intent {
            PlanProceedIntent::ContinuePlanning => {
                let mut history = base_history;
                history.push(PlanQaEntry {
                    question_id: "proposal_feedback".into(),
                    question_prompt: "Proposal feedback".into(),
                    user_input: user_input.clone(),
                    resolution: "User requested changes".into(),
                });
                let result = fetch_proposal(&backend, &flags, &initial, &history, &workspace).await;
                let _ = tx.send(UiUpdate::PlanLoopProposalDone(result)).await;
            }
            other => {
                let _ = tx.send(UiUpdate::PlanLoopProceedClassified(other)).await;
            }
        }
    });
}

/// True when the JSON plan loop owns user input (not the normal agent driver).
pub fn plan_loop_active(plan: &PlanState) -> bool {
    !matches!(
        plan.loop_phase,
        PlanLoopPhase::Idle
    )
}

/// Open the plan-mode entry confirmation modal (Y/N overlay, same chrome as babysitter approval).
pub fn open_plan_entry_dialog(
    app: &mut App,
    agent: &Arc<TokioMutex<Agent>>,
    goal: Option<String>,
) -> bool {
    if app.plan_entry_dialog_open() {
        app.left_committed
            .push("Plan entry already pending — press Y or N on the dialog.".to_string());
        return false;
    }

    let agent_mode = agent
        .try_lock()
        .map(|ag| ag.current_agent_mode())
        .unwrap_or_default();
    if agent_mode == "plan" {
        app.left_committed
            .push("Already in plan mode.".to_string());
        return false;
    }
    if app.plan.active && !app.plan.steps.is_empty() {
        app.left_committed
            .push("A plan is already executing — finish or /plan cancel first.".to_string());
        return false;
    }

    let user_goal = goal.as_ref().is_some_and(|g| !g.trim().is_empty());
    let goal_text = goal
        .filter(|g| !g.trim().is_empty())
        .unwrap_or_else(|| "Plan for the current task".to_string());

    app.plan.goal = goal_text.clone();
    reset_plan_for_new_request(&mut app.plan);

    if let Ok(mut ag) = agent.try_lock() {
        if let Some(s) = &mut ag.session_mut() {
            s.meta.current_goal = goal_text.clone();
            let _ = s.write_wiki_file("plan.md", &crate::plan_prompts::wiki_template_on_trigger(&goal_text));
            let _ = s.save_meta();
        }
    }

    app.pending_confirmation = Some(crate::confirmation_dialog::ConfirmationDialog::PlanEntry {
        goal: goal_text.clone(),
    });
    if user_goal {
        app.left_committed
            .push(format!("> {}", goal_text.trim()));
    }
    app.clear_input();
    app.history_index = None;
    app.needs_redraw = true;
    true
}

/// Respond to the plan-entry modal (Y/N keys).
pub fn apply_plan_entry_modal_response(
    app: &mut App,
    agent: &Arc<TokioMutex<Agent>>,
    goal: String,
    confirmed: bool,
) -> PlanSubmitOutcome {
    if !confirmed {
        app.left_committed
            .push("Plan mode entry cancelled.".to_string());
        app.needs_redraw = true;
        return PlanSubmitOutcome::Stop;
    }
    handle_plan_entry_confirmation(app, agent, "y", Some(goal))
}

/// Exit plan mode / clear pending entry ( `/plan cancel` ).
pub fn cancel_plan_mode(app: &mut App, agent: &Arc<TokioMutex<Agent>>) {
    if app.plan_entry_dialog_open() {
        app.pending_confirmation = None;
    }
    app.plan.recap_offered = false;

    let mut msg = "Plan mode cancelled.".to_string();
    if let Ok(mut ag) = agent.try_lock() {
        if ag.current_agent_mode() == "plan" {
            ag.set_agent_mode("talk");
            if let Some(s) = &mut ag.session_mut() {
                let _ = s.save_meta();
            }
            msg = "Exited plan mode (run mode → talk).".to_string();
        }
    }

    if app.plan.steps.is_empty() {
        app.plan.active = false;
        reset_plan_for_new_request(&mut app.plan);
    } else {
        app.plan.loop_phase = PlanLoopPhase::Idle;
        app.plan.pending_question = None;
        app.plan.pending_proposal = None;
    }
    app.left_committed.push(msg);
    app.needs_redraw = true;
}

/// Handle `/plan`, `/plan status`, `/plan cancel`, and `/plan <goal...>`.
pub fn dispatch_plan_slash(
    prompt: &str,
    app: &mut App,
    agent: &Arc<TokioMutex<Agent>>,
) -> bool {
    let parts: Vec<&str> = prompt
        .trim()
        .trim_start_matches('/')
        .split_whitespace()
        .collect();
    if parts.first().map(|p| p.eq_ignore_ascii_case("plan")) != Some(true) {
        return false;
    }

    match parts.get(1).map(|s| s.to_lowercase()).as_deref() {
        Some("status") => {
            let mode = agent
                .try_lock()
                .map(|ag| ag.current_agent_mode())
                .unwrap_or_else(|_| "unknown".to_string());
            app.left_committed
                .push(format_plan_status(&app.plan, &mode));
            app.needs_redraw = true;
            true
        }
        Some("cancel") => {
            cancel_plan_mode(app, agent);
            true
        }
        _ => {
            let goal = if parts.len() > 1 {
                Some(parts[1..].join(" "))
            } else {
                None
            };
            open_plan_entry_dialog(app, agent, goal)
        }
    }
}

/// After heuristic plan-trigger match, optionally disambiguate with a cheap LLM call.
pub async fn route_plan_entry_intent(
    app: &mut App,
    agent: &Arc<TokioMutex<Agent>>,
    backend: &Arc<Mutex<ChatBackend>>,
    flags: &RuntimeFlags,
    prompt: &str,
) -> PlanInputRouting {
    if app.plan.active || app.plan_entry_dialog_open() {
        return PlanInputRouting::Pass;
    }
    if !is_plan_trigger_phrase(prompt) {
        return PlanInputRouting::Pass;
    }

    let intent = classify_plan_entry_intent(backend, flags, prompt).await;
    match intent {
        PlanEntryIntent::Enter => {
            if open_plan_entry_dialog(app, agent, Some(prompt.to_string())) {
                PlanInputRouting::Stop
            } else {
                PlanInputRouting::Continue
            }
        }
        PlanEntryIntent::Chat => PlanInputRouting::Continue,
    }
}

/// When all steps are done: switch to talk mode and keep the pane until the next user message.
pub fn maybe_finalize_plan_execution(app: &mut App, agent: &Arc<TokioMutex<Agent>>) {
    if !app.plan.is_execution_complete() || app.plan.dismiss_pane_on_next_input {
        return;
    }
    app.plan.dismiss_pane_on_next_input = true;
    if let Ok(mut ag) = agent.try_lock() {
        if ag.current_agent_mode() == "work" {
            ag.set_agent_mode("talk");
            if let Some(s) = ag.session_mut() {
                let _ = s.save_meta();
            }
        }
        let mut exec = ag.plan_execution().clone();
        exec.active = false;
        ag.set_plan_execution(exec);
    }
    app.left_committed
        .push("✓ Plan complete — switched to talk mode.".to_string());
    app.needs_redraw = true;
}

/// Hide the plan pane after the user sends their next message post-completion.
pub fn dismiss_plan_pane_if_pending(app: &mut App) {
    if app.plan.dismiss_pane_on_next_input {
        app.plan.active = false;
        app.plan.dismiss_pane_on_next_input = false;
        app.needs_redraw = true;
    }
}

/// User approved the plan — switch to work mode, populate steps, return execution prompt.
pub fn start_plan_execution(
    app: &mut App,
    agent: &Arc<TokioMutex<Agent>>,
    workspace: &std::path::Path,
) -> String {
    approve_plan_for_execution(app, agent);
    app.plan.loop_phase = PlanLoopPhase::Idle;
    app.plan.pending_proposal = None;
    app.plan.recap_offered = false;
    app.left_committed
        .push("▶ Plan approved — starting execution.".to_string());
    format_plan_execution_user_prompt(&app.plan, workspace)
}

fn approve_plan_for_execution(app: &mut App, agent: &Arc<TokioMutex<Agent>>) {
    if let Ok(mut ag) = agent.try_lock() {
        ag.set_agent_mode("work");
        if let Some(s) = &mut ag.session_mut() {
            let _ = s.save_meta();
        }
    }
    app.plan.active = true;

    if app.plan.steps.is_empty() {
        populate_steps_on_proceed(app, agent);
    }

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

fn populate_steps_on_proceed(app: &mut App, agent: &Arc<TokioMutex<Agent>>) {
    let mut verif = app.plan.verification_steps.clone();
    if verif.is_empty() {
        verif = derive_verification_defaults(&app.plan.goal, VerificationDefaultsKind::ProceedFallback);
    }

    let plan_content = if let Ok(ag) = agent.try_lock() {
        ag.session()
            .as_ref()
            .and_then(|s| s.read_wiki_file_raw("plan.md").ok())
            .unwrap_or_default()
    } else {
        String::new()
    };

    let parsed = parse_plan_md(&plan_content);
    if !parsed.verification.is_empty() {
        verif = parsed.verification;
    }
    app.plan.verification_steps = verif.clone();

    let mut plan_steps = if !parsed.steps.is_empty() {
        parsed
            .steps
            .into_iter()
            .map(parsed_step_to_plan_step)
            .collect()
    } else {
        derive_fallback_steps(&app.plan.goal, &verif)
    };

    if !plan_steps.is_empty() {
        plan_steps[0].status = PlanStepStatus::InProgress;
        if !parsed.structured {
            if let Some(v0) = verif.first() {
                if plan_steps[0].verification.is_none()
                    && plan_steps[0].observe_prompt.is_none()
                {
                    plan_steps[0].verification = Some(v0.clone());
                }
            }
        }
        for step in &mut plan_steps {
            if step.tier.is_none() {
                step.tier = Some(PlanStepTier::Exec);
            }
        }
    }

    app.plan.steps = plan_steps;
    app.plan.current_step = 0;

    if let Ok(mut ag) = agent.try_lock() {
        if let Some(s) = ag.session_mut() {
            let _ = s.write_wiki_file("plan.md", &crate::plan_prompts::wiki_template_approved(&app.plan));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_various_plan_triggers() {
        assert!(is_plan_trigger_phrase(
            "come up with a plan to write a script"
        ));
        assert!(is_plan_trigger_phrase("let's plan this out"));
        assert!(is_plan_trigger_phrase("make a plan for the refactor"));
        assert!(is_plan_trigger_phrase("plan the implementation"));
        assert!(is_plan_trigger_phrase("create a plan for this task"));
        assert!(is_plan_trigger_phrase("what's the plan for the work"));
        assert!(is_plan_trigger_phrase("plan for this change"));
        assert!(is_plan_trigger_phrase("I want to plan the task"));
        assert!(is_plan_trigger_phrase("plan out the new feature"));
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
        assert!(!is_proceed_confirmation("maybe"));
        assert!(!is_proceed_confirmation("sounds good"));
    }

    #[test]
    fn verification_defaults_by_kind() {
        let py_goal = "write a python script";
        assert!(derive_verification_defaults(py_goal, VerificationDefaultsKind::PlanEntry)[0]
            .contains("<your_script>"));
        assert!(derive_verification_defaults(py_goal, VerificationDefaultsKind::ProceedFallback)[0]
            .contains("birthday_cake"));
        assert!(derive_verification_defaults(py_goal, VerificationDefaultsKind::AutoActivate)[0]
            .contains("<script>"));

        let rust_goal = "refactor the tui";
        let cargo = derive_verification_defaults(rust_goal, VerificationDefaultsKind::PlanEntry);
        assert_eq!(cargo[0], "cargo check");
    }

    #[test]
    fn parse_plan_md_extracts_verification_and_steps() {
        let md = r#"# Plan

**Verification:**
- cargo test
- cargo clippy

**Steps:**
1. Implement feature
2. Run tests
"#;
        let parsed = parse_plan_md(md);
        assert!(!parsed.structured);
        assert_eq!(parsed.verification, vec!["cargo test", "cargo clippy"]);
        assert_eq!(parsed.steps.len(), 2);
        assert_eq!(parsed.steps[0].description, "Implement feature");
        assert_eq!(parsed.steps[1].description, "Run tests");
    }

    #[test]
    fn parse_plan_md_prefers_structured_json_block() {
        let md = r#"# Plan

## Verification
- cargo check

## Steps
1. Legacy step should be ignored

<!-- plan-steps:json
[
  {
    "description": "Implement foo in bar.rs",
    "tier": "exec",
    "verification": "cargo test -p raven-tui plan_flow"
  },
  {
    "description": "Confirm LED visible on device",
    "tier": "observe",
    "prompt": "Did the status LED blink? (yes/no)",
    "note": "Hardware not readable from shell"
  }
]
-->
"#;
        let parsed = parse_plan_md(md);
        assert!(parsed.structured);
        assert_eq!(parsed.steps.len(), 2);
        assert_eq!(parsed.steps[0].description, "Implement foo in bar.rs");
        assert_eq!(parsed.steps[0].tier.as_deref(), Some("exec"));
        assert_eq!(
            parsed.steps[0].verification.as_deref(),
            Some("cargo test -p raven-tui plan_flow")
        );
        assert_eq!(parsed.steps[1].tier.as_deref(), Some("observe"));
        assert_eq!(
            parsed.steps[1].prompt.as_deref(),
            Some("Did the status LED blink? (yes/no)")
        );
    }

    #[test]
    fn structured_steps_map_to_plan_state_on_proceed() {
        let json = r#"[
  {"description": "Run tests", "tier": "exec", "verification": "cargo test"},
  {"description": "Check UI", "tier": "observe", "prompt": "Does it look right?"}
]"#;
        let parsed_steps = parse_plan_steps_json(json).expect("json");
        let plan_steps: Vec<PlanStep> = parsed_steps
            .into_iter()
            .map(super::parsed_step_to_plan_step)
            .collect();
        assert_eq!(plan_steps[0].tier, Some(PlanStepTier::Exec));
        assert_eq!(
            plan_steps[0].verification.as_deref(),
            Some("cargo test")
        );
        assert_eq!(plan_steps[1].tier, Some(PlanStepTier::Observe));
        assert_eq!(
            plan_steps[1].observe_prompt.as_deref(),
            Some("Does it look right?")
        );
    }

    #[test]
    fn derive_fallback_steps_three_phases() {
        let steps = derive_fallback_steps("fix the bug", &["cargo test".to_string()]);
        assert_eq!(steps.len(), 3);
        assert!(steps[0].description.contains("Implement"));
    }

    #[test]
    fn plan_entry_modal_view_has_goal_and_prompt() {
        let dialog = crate::confirmation_dialog::ConfirmationDialog::PlanEntry {
            goal: "Build Galaga".to_string(),
        };
        let view = dialog.view();
        assert_eq!(view.headline, "Enter plan mode?");
        assert_eq!(view.detail, "Build Galaga");
        assert_eq!(view.title, " Plan Mode ");
    }

    #[test]
    fn nl_plan_trigger_still_supported() {
        assert!(is_plan_trigger_phrase("let's do a plan for this feature"));
        assert!(is_plan_trigger_phrase("come up with a plan"));
    }

    #[test]
    fn negated_plan_meta_question_matches_trigger_but_not_entry() {
        let msg = "Don't enter plan mode, but analyze how plan mode works";
        assert!(is_plan_trigger_phrase(msg));
        assert!(raven_tui::plan_intent::is_plan_entry_negated(msg));
    }

    #[test]
    fn detect_plan_recap_invite_phrases() {
        assert!(detect_plan_recap_invite(
            "Shall we proceed with this plan, or would you like to make a change?"
        ));
        assert!(!detect_plan_recap_invite("Where should the project live?"));
    }

    #[test]
    fn galaga_planning_prompt_is_trigger_not_proceed() {
        let msg = "let's work on a plan to create a c++ program similar to the galaga video game";
        assert!(is_plan_trigger_phrase(msg));
        assert!(raven_tui::plan_intent::is_planning_request_message(msg));
        assert!(!is_proceed_confirmation(msg));
    }
}