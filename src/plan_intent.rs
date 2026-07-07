//! Cheap LLM classifiers for plan-mode entry and proceed decisions.
//!
//! Heuristic substring checks are the fast gate; when they fire (or during plan
//! mode for proceed), a small non-streaming call disambiguates meta-questions,
//! negation, and soft confirmations.

use crate::chat_backend::ChatBackend;
use crate::llm::{ChatRequest, Message};
use crate::plan_protocol::PlanQuestion;
use crate::runtime::RuntimeFlags;
use std::sync::Arc;
use tokio::sync::Mutex;

const CLASSIFY_MAX_TOKENS: u32 = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanEntryIntent {
    /// User wants to enter plan mode for a task.
    Enter,
    /// Normal chat — meta questions, negation, unrelated ask.
    Chat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanProceedIntent {
    /// User approved the plan; switch to work mode.
    Proceed,
    /// Keep clarifying / negotiating the plan.
    ContinuePlanning,
    /// User wants to abandon plan mode.
    Cancel,
}

/// Obvious negation — cheap pre-filter before LLM.
pub fn is_plan_entry_negated(message: &str) -> bool {
    let lower = message.to_lowercase();
    [
        "don't enter plan",
        "do not enter plan",
        "don't use plan mode",
        "do not use plan mode",
        "without entering plan",
        "not enter plan mode",
        "don't switch to plan",
        "do not switch to plan",
    ]
    .iter()
    .any(|p| lower.contains(p))
}

pub fn parse_entry_intent_line(text: &str) -> Option<PlanEntryIntent> {
    let first = text.lines().next()?.trim().to_uppercase();
    if first.starts_with("ENTER") {
        Some(PlanEntryIntent::Enter)
    } else if first.starts_with("CHAT") {
        Some(PlanEntryIntent::Chat)
    } else {
        None
    }
}

pub fn parse_proceed_intent_line(text: &str) -> Option<PlanProceedIntent> {
    let first = text.lines().next()?.trim().to_uppercase();
    if first.starts_with("PROCEED") {
        Some(PlanProceedIntent::Proceed)
    } else if first.starts_with("CONTINUE") {
        Some(PlanProceedIntent::ContinuePlanning)
    } else if first.starts_with("CANCEL") {
        Some(PlanProceedIntent::Cancel)
    } else {
        None
    }
}

fn entry_heuristic_fallback(message: &str) -> PlanEntryIntent {
    if is_plan_entry_negated(message) {
        PlanEntryIntent::Chat
    } else {
        PlanEntryIntent::Enter
    }
}

/// True when the message is asking to *start* planning a task, not approving a recap.
pub fn is_planning_request_message(message: &str) -> bool {
    let lower = message.to_lowercase();
    [
        "work on a plan",
        "let's plan",
        "lets plan",
        "make a plan",
        "create a plan",
        "come up with a plan",
        "develop a plan",
        "plan out",
        "plan for this",
        "plan the",
        "what's the plan",
    ]
    .iter()
    .any(|p| lower.contains(p))
        || (lower.contains("plan")
            && (lower.contains("work on")
                || lower.contains("implement")
                || lower.contains("refactor")
                || lower.contains("create a")))
}

/// User is answering a clarification question, not approving the full plan.
pub fn is_clarification_response(message: &str) -> bool {
    let lower = message.trim().to_lowercase();
    if lower.len() <= 3 && lower.chars().all(|c| c.is_ascii_digit()) {
        return true;
    }
    if lower.starts_with("option ") || lower.starts_with('#') {
        return true;
    }
    [
        "how about",
        "what about",
        "i prefer",
        "i'd prefer",
        "let's use",
        "lets use",
        "subdirectory",
        "separate directory",
        "go with",
        "instead of",
        "rather than",
        "rather use",
        "use sfml",
        "use sdl",
        "./",
        "here ./",
    ]
    .iter()
    .any(|p| lower.contains(p))
}

/// Hard proceed phrases — safe without LLM when recap was already offered.
pub fn is_explicit_proceed_approval(message: &str) -> bool {
    let lower = message.trim().to_lowercase();
    lower.contains("proceed")
        || lower.contains("go ahead")
        || lower.contains("start executing")
        || lower.contains("let's do it")
        || lower.contains("lets do it")
        || lower == "do it"
        || lower.contains("go for it")
        || lower.contains("confirmed")
}

/// Soft assent that needs LLM/disambiguation after a recap invite.
pub fn is_soft_proceed_candidate(message: &str) -> bool {
    let lower = message.trim().to_lowercase();
    lower == "yes" || lower == "y"
        || lower.contains("sounds good")
        || lower.contains("looks good")
        || lower.contains("let's go")
        || lower.contains("lets go")
}

/// Heuristic proceed fallback (used when LLM disabled/unavailable).
pub fn proceed_heuristic_fallback(message: &str) -> PlanProceedIntent {
    if is_planning_request_message(message) || is_clarification_response(message) {
        return PlanProceedIntent::ContinuePlanning;
    }
    let lower = message.trim().to_lowercase();
    if lower == "no"
        || lower.starts_with("no ")
        || lower.contains("cancel")
        || lower.contains("abort")
        || lower.contains("stop planning")
    {
        return PlanProceedIntent::Cancel;
    }
    if lower.contains("i don't know")
        || lower.contains("not sure")
        || lower.contains("maybe")
        || lower.contains("can we change")
        || lower.contains("wait")
    {
        return PlanProceedIntent::ContinuePlanning;
    }
    if is_explicit_proceed_approval(message) || is_soft_proceed_candidate(message) {
        return PlanProceedIntent::Proceed;
    }
    PlanProceedIntent::ContinuePlanning
}

async fn classify_chat(
    backend: &Arc<Mutex<ChatBackend>>,
    prompt: String,
) -> Option<String> {
    let req = ChatRequest {
        messages: vec![Message {
            role: "user".into(),
            content: Some(prompt),
            tool_calls: None,
            tool_call_id: None,
        }],
        tools: None,
        temperature: 0.0,
        max_tokens: CLASSIFY_MAX_TOKENS,
        stream: false,
        reasoning_enabled: None,
        json_object_mode: None,
    };
    let resp = backend.lock().await.chat(req).await.ok()?;
    if resp.content.trim().is_empty() {
        None
    } else {
        Some(resp.content)
    }
}

fn use_llm_classifier(flags: &crate::runtime::RuntimeFlags, backend: &ChatBackend) -> bool {
    flags.plan_intent_llm && !flags.is_eval && !matches!(backend, ChatBackend::Mock(_))
}

/// After a heuristic plan trigger match, decide if the user really wants plan mode.
pub async fn classify_plan_entry_intent(
    backend: &Arc<Mutex<ChatBackend>>,
    flags: &crate::runtime::RuntimeFlags,
    user_message: &str,
) -> PlanEntryIntent {
    if is_plan_entry_negated(user_message) {
        return PlanEntryIntent::Chat;
    }
    {
        let guard = backend.lock().await;
        if !use_llm_classifier(flags, &guard) {
            return entry_heuristic_fallback(user_message);
        }
    }

    let prompt = format!(
        "You classify user intent for a coding-agent TUI.\n\
         Plan mode = structured clarification (goal, verification, steps) BEFORE any implementation.\n\n\
         User message:\n\"{user_message}\"\n\n\
         Reply with exactly one word on the first line:\n\
         ENTER — user wants to plan a coding/task (enter plan mode)\n\
         CHAT — user is asking ABOUT plan mode, declining it, or wants a normal answer without entering plan mode"
    );

    match classify_chat(backend, prompt).await {
        Some(text) => parse_entry_intent_line(&text).unwrap_or_else(|| entry_heuristic_fallback(user_message)),
        None => entry_heuristic_fallback(user_message),
    }
}

/// How the user answered a structured plan clarification question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanAnswerResolution {
    Selected { option_id: String, label: String },
    FreeText(String),
    DeferToRecommend,
    ExitDiscuss,
    ReviseProposal,
}

pub fn parse_answer_intent_line(text: &str) -> Option<PlanAnswerResolution> {
    let first = text.lines().next()?.trim().to_uppercase();
    if first.starts_with("SELECT") {
        let id = text
            .lines()
            .nth(1)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())?;
        Some(PlanAnswerResolution::Selected {
            option_id: id.clone(),
            label: id,
        })
    } else if first.starts_with("FREE") {
        let body = text.lines().skip(1).collect::<Vec<_>>().join(" ").trim().to_string();
        if body.is_empty() {
            None
        } else {
            Some(PlanAnswerResolution::FreeText(body))
        }
    } else if first.starts_with("DEFER") {
        Some(PlanAnswerResolution::DeferToRecommend)
    } else if first.starts_with("EXIT") {
        Some(PlanAnswerResolution::ExitDiscuss)
    } else if first.starts_with("REVISE") {
        Some(PlanAnswerResolution::ReviseProposal)
    } else {
        None
    }
}

fn answer_heuristic(message: &str, question: &PlanQuestion) -> PlanAnswerResolution {
    let lower = message.trim().to_lowercase();
    if lower.contains("let's discuss")
        || lower.contains("lets discuss")
        || lower.contains("talk about")
        || lower.contains("exit plan")
        || lower.contains("leave plan")
    {
        return PlanAnswerResolution::ExitDiscuss;
    }
    if lower.contains("you pick")
        || lower.contains("your choice")
        || lower.contains("up to you")
        || lower == "recommend"
        || lower.contains("go with your recommend")
    {
        return PlanAnswerResolution::DeferToRecommend;
    }
    if let Some(digit) = lower.chars().find(|c| c.is_ascii_digit()) {
        if let Some(idx) = digit.to_digit(10).map(|d| d as usize).filter(|&d| d >= 1) {
            if let Some(opt) = question.options.get(idx - 1) {
                return PlanAnswerResolution::Selected {
                    option_id: opt.id.clone(),
                    label: opt.label.clone(),
                };
            }
        }
    }
    for opt in &question.options {
        let id_lower = opt.id.to_lowercase();
        let label_lower = opt.label.to_lowercase();
        if lower == id_lower
            || lower.contains(&id_lower)
            || lower.contains(&label_lower)
            || lower.contains("first") && question.options.first().map(|o| o.id.as_str()) == Some(opt.id.as_str())
            || lower.contains("second") && question.options.get(1).map(|o| o.id.as_str()) == Some(opt.id.as_str())
        {
            return PlanAnswerResolution::Selected {
                option_id: opt.id.clone(),
                label: opt.label.clone(),
            };
        }
    }
    PlanAnswerResolution::FreeText(message.trim().to_string())
}

/// Map freeform user input to a structured answer for the active question.
pub async fn classify_plan_answer(
    backend: &Arc<Mutex<ChatBackend>>,
    flags: &RuntimeFlags,
    user_message: &str,
    question: &PlanQuestion,
) -> PlanAnswerResolution {
    if user_message.trim().is_empty() {
        return PlanAnswerResolution::FreeText(String::new());
    }
    {
        let guard = backend.lock().await;
        if !use_llm_classifier(flags, &guard) {
            return answer_heuristic(user_message, question);
        }
    }
    let options_lines = question
        .options
        .iter()
        .enumerate()
        .map(|(i, o)| format!("  {}. id={} label={}", i + 1, o.id, o.label))
        .collect::<Vec<_>>()
        .join("\n");
    let rec = question.recommend.as_deref().unwrap_or("(none)");
    let prompt = format!(
        "The user is answering a plan clarification question.\n\n\
         Question: {q}\n\
         Options:\n{options_lines}\n\
         Recommended option id: {rec}\n\n\
         User message:\n\"{user_message}\"\n\n\
         Reply with exactly one token on line 1, then details on line 2 if needed:\n\
         SELECT — user picked an option (line 2 = option id)\n\
         FREE — free-text answer (line 2 = text)\n\
         DEFER — user wants the recommended/default choice\n\
         EXIT — user wants to leave structured plan mode and discuss freely\n\
         REVISE — user wants to change the proposed plan (only valid after proposal, treat as FREE here)",
        q = question.prompt,
    );
    match classify_chat(backend, prompt).await {
        Some(text) => parse_answer_intent_line(&text).unwrap_or_else(|| answer_heuristic(user_message, question)),
        None => answer_heuristic(user_message, question),
    }
}

/// Classify consent after the harness showed the final proposal.
pub async fn classify_proposal_consent(
    backend: &Arc<Mutex<ChatBackend>>,
    flags: &RuntimeFlags,
    user_message: &str,
) -> PlanProceedIntent {
    if is_explicit_proceed_approval(user_message) {
        return PlanProceedIntent::Proceed;
    }
    if user_message.trim().to_lowercase().contains("cancel") {
        return PlanProceedIntent::Cancel;
    }
    if is_soft_proceed_candidate(user_message) {
        classify_plan_proceed_intent(backend, flags, user_message, "").await
    } else if user_message.contains('?')
        || user_message.to_lowercase().contains("change")
        || user_message.to_lowercase().contains("what about")
    {
        PlanProceedIntent::ContinuePlanning
    } else {
        classify_plan_proceed_intent(backend, flags, user_message, "").await
    }
}

/// While in plan mode, classify whether the user is approving execution.
pub async fn classify_plan_proceed_intent(
    backend: &Arc<Mutex<ChatBackend>>,
    flags: &crate::runtime::RuntimeFlags,
    user_message: &str,
    plan_goal: &str,
) -> PlanProceedIntent {
    if is_planning_request_message(user_message) {
        return PlanProceedIntent::ContinuePlanning;
    }

    {
        let guard = backend.lock().await;
        if !use_llm_classifier(flags, &guard) {
            return proceed_heuristic_fallback(user_message);
        }
    }

    let goal_line = if plan_goal.trim().is_empty() {
        String::new()
    } else {
        format!("Current plan goal: {plan_goal}\n\n")
    };

    let prompt = format!(
        "The user is in plan mode (clarification / recap). They may be responding to a proposed plan or verification steps.\n\n\
         {goal_line}\
         User message:\n\"{user_message}\"\n\n\
         Reply with exactly one word on the first line:\n\
         PROCEED — user approves an already-presented plan and wants implementation to start now (short assent: \"proceed\", \"go ahead\", \"yes\", \"let's do it\", \"sounds good\")\n\
         CONTINUE — user is still planning, asking questions, proposing a new/changed goal, or describing what to plan (e.g. \"let's work on a plan for…\", \"what about step 3\", \"I don't know\")\n\
         CANCEL — user wants to abandon planning"
    );

    match classify_chat(backend, prompt).await {
        Some(text) => {
            parse_proceed_intent_line(&text).unwrap_or_else(|| proceed_heuristic_fallback(user_message))
        }
        None => proceed_heuristic_fallback(user_message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negation_blocks_entry_heuristic() {
        assert!(is_plan_entry_negated(
            "Don't enter plan mode, but analyze how plan mode works"
        ));
        assert_eq!(
            entry_heuristic_fallback("Don't enter plan mode, but analyze how plan mode works"),
            PlanEntryIntent::Chat
        );
    }

    #[test]
    fn parse_entry_and_proceed_lines() {
        assert_eq!(
            parse_entry_intent_line("ENTER\nbecause task planning"),
            Some(PlanEntryIntent::Enter)
        );
        assert_eq!(
            parse_proceed_intent_line("CONTINUE\nuser unsure"),
            Some(PlanProceedIntent::ContinuePlanning)
        );
    }

    #[test]
    fn proceed_heuristic_differentiates_uncertainty() {
        assert_eq!(
            proceed_heuristic_fallback("I don't know about step 3"),
            PlanProceedIntent::ContinuePlanning
        );
        assert_eq!(
            proceed_heuristic_fallback("let's do it"),
            PlanProceedIntent::Proceed
        );
    }

    #[test]
    fn planning_request_is_not_proceed_approval() {
        let msg = "let's work on a plan to create a c++ program similar to the galaga video game";
        assert!(is_planning_request_message(msg));
        assert_eq!(
            proceed_heuristic_fallback(msg),
            PlanProceedIntent::ContinuePlanning
        );
    }

    #[test]
    fn clarification_answer_is_not_proceed() {
        let msg = "how about a subdirectory here ./galaga";
        assert!(is_clarification_response(msg));
        assert!(!is_explicit_proceed_approval(msg));
        assert_eq!(
            proceed_heuristic_fallback(msg),
            PlanProceedIntent::ContinuePlanning
        );
    }

    #[test]
    fn bare_yes_is_soft_not_explicit_proceed() {
        assert!(is_soft_proceed_candidate("yes"));
        assert!(!is_explicit_proceed_approval("yes"));
    }

    #[test]
    fn answer_heuristic_maps_digit_to_option() {
        use crate::plan_protocol::{PlanQuestion, PlanQuestionOption};
        let q = PlanQuestion {
            id: "loc".into(),
            prompt: "Where?".into(),
            kind: "choice".into(),
            options: vec![
                PlanQuestionOption {
                    id: "subdir".into(),
                    label: "Subdir".into(),
                },
                PlanQuestionOption {
                    id: "sep".into(),
                    label: "Separate".into(),
                },
            ],
            recommend: Some("subdir".into()),
            allow_free_text: true,
        };
        match answer_heuristic("2", &q) {
            PlanAnswerResolution::Selected { option_id, .. } => assert_eq!(option_id, "sep"),
            other => panic!("expected select, got {other:?}"),
        }
    }
}