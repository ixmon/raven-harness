//! Harness-driven plan mode loop: JSON clarify → JSON proposal → user consent.

use crate::chat_backend::ChatBackend;
use crate::llm::{ChatRequest, Message};
use crate::llm::ChatResponse;
use crate::plan_protocol::{
    format_qa_history_for_prompt, looks_truncated_json, parse_plan_model_payload,
    text_for_plan_json_parse, PlanModelPayload, PlanQaEntry, PlanQuestion,
    PlanQuestionOption,
};
use crate::plan_verification::{
    format_adversarial_critique_nudge, format_project_workdir_prompt_section,
    format_validation_retry_nudge, improve_proposal, resolve_project_workdir,
    resolve_project_workdir_from_context,
};
use crate::runtime::RuntimeFlags;
use std::sync::Arc;
use tokio::sync::Mutex;

const CLARIFY_MAX_TOKENS: u32 = 4096;
const PROPOSAL_MAX_TOKENS: u32 = 8192;

fn use_plan_loop_llm(flags: &RuntimeFlags, backend: &ChatBackend) -> bool {
    !flags.is_eval && !matches!(backend, ChatBackend::Mock(_))
}

const PARSE_RETRY_NUDGE: &str = "\n\nYour previous reply was not valid JSON. Reply with ONLY one raw JSON object starting with {. No markdown fences, no prose.";

const CLARIFY_CONCISE_NUDGE: &str = "\n\nYour previous clarify JSON was truncated. Reply with ONLY compact JSON: question.prompt under 100 characters, at most 4 options with labels under 35 characters each. No examples in parentheses — use short labels like \"Minimal prototype\" / \"Full feature set\".";

// Shrink steps/examples on truncation — keep success_criteria complete enough to
// cover product outcomes (not crushed to a 100-char build-only line).
const PROPOSAL_CONCISE_NUDGE: &str = "\n\nYour previous proposal JSON was truncated (too long). Reply with ONLY one compact JSON object: goal under 120 characters; success_criteria under 400 characters and still product-level (cover capabilities named in the goal, not only \"builds\"); at most 8 steps; step descriptions under 60 characters; short verification commands only. Omit rollback and constraints unless essential.";

async fn plan_loop_chat(
    backend: &Arc<Mutex<ChatBackend>>,
    system: &str,
    user: &str,
    max_tokens: u32,
) -> Result<ChatResponse, String> {
    let req = ChatRequest {
        messages: vec![
            Message {
                role: "system".into(),
                content: Some(system.to_string()),
                tool_calls: None,
                tool_call_id: None,
            },
            Message {
                role: "user".into(),
                content: Some(user.to_string()),
                tool_calls: None,
                tool_call_id: None,
            },
        ],
        tools: None,
        temperature: 0.2,
        max_tokens,
        stream: false,
        reasoning_enabled: Some(false),
        json_object_mode: Some(true),
    };
    let resp = backend
        .lock()
        .await
        .chat(req)
        .await
        .map_err(|e| format!("plan loop LLM error: {e}"))?;
    let has_content = !resp.content.trim().is_empty();
    let has_reasoning = resp
        .reasoning_content
        .as_deref()
        .is_some_and(|s| !s.trim().is_empty());
    if has_content || has_reasoning {
        Ok(resp)
    } else {
        Err("empty plan loop response".to_string())
    }
}

async fn fetch_plan_payload(
    backend: &Arc<Mutex<ChatBackend>>,
    system: &str,
    user: &str,
    max_tokens: u32,
    concise_nudge: Option<&str>,
) -> Result<PlanModelPayload, String> {
    let mut prompts = vec![
        user.to_string(),
        format!("{user}{PARSE_RETRY_NUDGE}"),
    ];
    if let Some(nudge) = concise_nudge {
        prompts.push(format!("{user}{nudge}"));
    }
    let mut last_err = String::new();
    for prompt in &prompts {
        let resp = plan_loop_chat(backend, system, prompt, max_tokens).await?;
        let truncated = resp.finish_reason.as_deref() == Some("length");
        let text = text_for_plan_json_parse(&resp.content, resp.reasoning_content.as_deref());
        match parse_plan_model_payload(&text) {
            Ok(payload) => return Ok(payload),
            Err(e) => {
                let was_truncated = truncated || looks_truncated_json(&text);
                last_err = if truncated {
                    format!("{e} (finish_reason=length)")
                } else {
                    e
                };
                if was_truncated && prompt == prompts.last().unwrap() {
                    break;
                }
            }
        }
    }
    Err(last_err)
}

fn fallback_clarify_question() -> PlanQuestion {
    PlanQuestion {
        id: "scope".into(),
        prompt: "Start minimal or aim for a fuller feature set?".into(),
        kind: "choice".into(),
        options: vec![
            PlanQuestionOption {
                id: "minimal".into(),
                label: "Minimal prototype".into(),
            },
            PlanQuestionOption {
                id: "full".into(),
                label: "Fuller feature set".into(),
            },
        ],
        recommend: Some("minimal".into()),
        allow_free_text: true,
    }
}

fn is_truncation_error(err: &str) -> bool {
    err.contains("truncated JSON") || err.contains("finish_reason=length")
}

const CLARIFY_SYSTEM: &str = r#"You are the planning engine for a coding-agent TUI. Reply with JSON ONLY — no markdown outside a single JSON object, no prose.

Pick the single most important unresolved decision for the user's task. Run mental environment audit: if recommending libraries (SFML, SDL, etc.), note that probes should happen before final proposal.

Response types:
1. {"type":"clarify","question":{...}} — one question only
   question fields: id, prompt, kind ("choice"|"text"), options[{id,label}] (for choice), recommend (option id), allow_free_text (bool)
2. {"type":"ready","message":"..."} — no important decision left to clarify; harness will ask for full proposal next

Rules:
- Keep JSON tiny: question.prompt <= 100 chars, option labels <= 35 chars, max 4 options
- No long parenthetical examples in prompt or labels — use short labels
- Exactly ONE question per response when type is clarify
- recommend must match an option id when options are present
- If nothing important remains, use type ready"#;

fn proposal_system_prompt() -> String {
    let recipes = crate::plan_recipes::format_recipe_card_for_prompt();
    format!(
        r#"You are the planning engine for a coding-agent TUI. Reply with JSON ONLY — one object, no extra prose.

Emit the final plan:
{{"type":"proposal","goal":"...","success_criteria":"...","verification":["runnable command",...],"rollback":"...","constraints":"...","steps":[{{"description":"...","tier":"exec|check|attested|observe","verification":"...","prompt":"...","note":"..."}}]}}

success_criteria vs verification (critical):
- `success_criteria` = user-observable acceptance outcomes (what must be true when the work is done). NOT only "builds" / "compiles" / "runs".
- Cover every major capability named in the goal or initial request (e.g. fire, collide, explode, API endpoint, UI panel). Prefer a short semicolon-separated list of testable claims.
- Example: "Player can fire; bullets hit enemies; enemies explode; game builds and runs under 5s smoke"
- Bad: "cmake --build build" or "Build and run" alone when the goal names features.
- Keep success_criteria under ~400 characters; do not empty it to save space.
- `verification` = separate runnable commands that prove as much of those outcomes as practical (build, tests, greps). Do not put shell commands in success_criteria; put them in verification / step.verification.

Verification rules (critical):
- Verification PROVES the step outcome — never replay how you would create files.
- NEVER use as verification: `cat >`, `echo >`, `tee`, `touch`, bare `mkdir` / `mkdir -p`.
- Prefer the harness recipes below (paths relative to project root / project_workdir).
- Run/smoke test → tier `exec` with timeout-safe command (not an infinite game loop).
- `attested` only when no practical check exists — require `note` explaining why.
- `observe` for human-only checks — put question in `prompt`, explain in `note`.

Example steps for a C++ game in subdirectory `galaga/`:
{{"description":"Create CMakeLists.txt","tier":"check","verification":"min_bytes:CMakeLists.txt:40"}}
{{"description":"Create src/player.cpp","tier":"check","verification":"min_bytes:src/player.cpp:80"}}
{{"description":"Compile project","tier":"exec","verification":"cmake --build build"}}

{recipes}

Other rules:
- Keep JSON compact: short step strings, max 10 steps, omit optional fields when empty — but never drop product outcomes from success_criteria
- When the user names a project subdirectory (e.g. `./galaga/`), that directory IS the project root — all files and verifications are relative to it (`src/foo.cpp`, not workspace-root paths)
- If a **User-specified project directory** block appears below, follow it exactly and record it in `constraints`
- Every step needs tier + verification (or `prompt` for observe)
- Include package-install steps if user chose agent install"#
    )
}

fn clarify_user_prompt(
    initial_request: &str,
    history_text: &str,
    workspace: &str,
    project_workdir: Option<&str>,
) -> String {
    let workdir_section = project_workdir
        .map(format_project_workdir_prompt_section)
        .unwrap_or_default();
    format!(
        "Workspace: {workspace}\n\nInitial user request:\n{initial_request}\n\nPrior clarifications:\n{history_text}{workdir_section}\n\nEmit the next JSON response."
    )
}

fn proposal_user_prompt(
    initial_request: &str,
    history_text: &str,
    workspace: &str,
    project_workdir: Option<&str>,
) -> String {
    let workdir_section = project_workdir
        .map(format_project_workdir_prompt_section)
        .unwrap_or_default();
    format!(
        "Workspace: {workspace}\n\nInitial user request:\n{initial_request}\n\nClarifications resolved:\n{history_text}{workdir_section}\n\nEmit the full proposal JSON."
    )
}

/// Ask the model for the next clarification question (or ready).
pub async fn fetch_clarification(
    backend: &Arc<Mutex<ChatBackend>>,
    flags: &RuntimeFlags,
    initial_request: &str,
    history: &[PlanQaEntry],
    workspace: &str,
) -> Result<PlanModelPayload, String> {
    {
        let guard = backend.lock().await;
        if !use_plan_loop_llm(flags, &guard) {
            return Err("plan loop requires a live LLM (disabled in eval/mock)".to_string());
        }
    }
    let history_text = format_qa_history_for_prompt(history);
    let workdir_hint = resolve_project_workdir_from_context(initial_request, history);
    let user = clarify_user_prompt(
        initial_request,
        &history_text,
        workspace,
        workdir_hint.as_deref(),
    );
    match fetch_plan_payload(
        backend,
        CLARIFY_SYSTEM,
        &user,
        CLARIFY_MAX_TOKENS,
        Some(CLARIFY_CONCISE_NUDGE),
    )
    .await
    {
        Ok(payload) => Ok(payload),
        Err(e) if is_truncation_error(&e) => Ok(PlanModelPayload::Clarify {
            question: fallback_clarify_question(),
        }),
        Err(e) => Err(e),
    }
}

async fn fetch_proposal_payload(
    backend: &Arc<Mutex<ChatBackend>>,
    user: &str,
) -> Result<PlanModelPayload, String> {
    let system = proposal_system_prompt();
    fetch_plan_payload(
        backend,
        &system,
        user,
        PROPOSAL_MAX_TOKENS,
        Some(PROPOSAL_CONCISE_NUDGE),
    )
    .await
}

/// Ask the model for the final plan proposal JSON.
pub async fn fetch_proposal(
    backend: &Arc<Mutex<ChatBackend>>,
    flags: &RuntimeFlags,
    initial_request: &str,
    history: &[PlanQaEntry],
    workspace: &str,
) -> Result<PlanModelPayload, String> {
    {
        let guard = backend.lock().await;
        if !use_plan_loop_llm(flags, &guard) {
            return Err("plan loop requires a live LLM (disabled in eval/mock)".to_string());
        }
    }
    let history_text = format_qa_history_for_prompt(history);
    let workdir_hint = resolve_project_workdir_from_context(initial_request, history);
    let user = proposal_user_prompt(
        initial_request,
        &history_text,
        workspace,
        workdir_hint.as_deref(),
    );
    let payload = fetch_proposal_payload(backend, &user).await?;
    let PlanModelPayload::Proposal(mut proposal) = payload else {
        return Ok(payload);
    };

    let workdir = resolve_project_workdir(initial_request, history, &proposal.steps);
    let first_report = improve_proposal(&mut proposal, workdir.as_deref());

    // Case 1: blocking errors → retry with structural rules nudge.
    if !first_report.errors.is_empty() {
        let nudge = format_validation_retry_nudge(&first_report.errors);
        let retry_user = format!("{user}{nudge}");
        return Ok(match fetch_proposal_payload(backend, &retry_user).await {
            Ok(PlanModelPayload::Proposal(mut retry)) => {
                let retry_workdir =
                    resolve_project_workdir(initial_request, history, &retry.steps);
                let retry_report = improve_proposal(&mut retry, retry_workdir.as_deref());
                if retry_report.errors.len() <= first_report.errors.len() {
                    PlanModelPayload::Proposal(retry)
                } else {
                    PlanModelPayload::Proposal(proposal)
                }
            }
            Ok(other) => other,
            Err(_) => PlanModelPayload::Proposal(proposal),
        });
    }

    // Case 2: no errors but warnings → adversarial critique nudge. The
    // hardcoded lints are language-specific seeds (download/empty-dir,
    // grep-in-cpp, pipe-masking); this asks the model to harden its own
    // verifications using its knowledge of THIS project's stack, so the fix
    // generalizes to languages/platforms the harness has no rules for.
    if !first_report.warnings.is_empty() {
        let nudge = format_adversarial_critique_nudge(&first_report.warnings);
        let retry_user = format!("{user}{nudge}");
        if let Ok(PlanModelPayload::Proposal(mut retry)) =
            fetch_proposal_payload(backend, &retry_user).await
        {
            let retry_workdir =
                resolve_project_workdir(initial_request, history, &retry.steps);
            let retry_report = improve_proposal(&mut retry, retry_workdir.as_deref());
            // Accept the hardened proposal only if it introduced no errors and
            // reduced (or at least did not increase) the warnings.
            if retry_report.errors.is_empty()
                && retry_report.warnings.len() <= first_report.warnings.len()
            {
                return Ok(PlanModelPayload::Proposal(retry));
            }
        }
        // Otherwise keep the original; warnings are non-blocking and will be
        // surfaced to the user in the proposal recap.
    }

    Ok(PlanModelPayload::Proposal(proposal))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proposal_system_prompt_teaches_product_level_success_criteria() {
        let p = proposal_system_prompt();
        assert!(p.contains("success_criteria vs verification"));
        assert!(p.contains("user-observable acceptance outcomes"));
        assert!(
            p.contains("Do not put shell commands in success_criteria"),
            "must separate criteria prose from shell verification"
        );
        assert!(p.contains("~400 characters"));
    }

    #[test]
    fn proposal_concise_nudge_preserves_room_for_criteria() {
        assert!(PROPOSAL_CONCISE_NUDGE.contains("400 characters"));
        assert!(
            !PROPOSAL_CONCISE_NUDGE.contains("success_criteria under 100 characters"),
            "100-char cap crushed product criteria"
        );
        assert!(PROPOSAL_CONCISE_NUDGE.contains("product-level"));
    }
}
