//! Harness-driven plan mode loop: JSON clarify → JSON proposal → user consent.

use crate::chat_backend::ChatBackend;
use crate::llm::{ChatRequest, Message};
use crate::llm::ChatResponse;
use crate::plan_protocol::{
    format_qa_history_for_prompt, looks_truncated_json, parse_plan_model_payload,
    text_for_plan_json_parse, PlanModelPayload, PlanQaEntry, PlanQuestion,
    PlanQuestionOption,
};
use crate::plan_verification::{format_validation_retry_nudge, improve_proposal};
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

const PROPOSAL_CONCISE_NUDGE: &str = "\n\nYour previous proposal JSON was truncated (too long). Reply with ONLY one compact JSON object: keep goal and success_criteria under 100 characters each, at most 8 steps, step descriptions under 60 characters, short verification commands only. Omit rollback and constraints unless essential.";

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

const PROPOSAL_SYSTEM: &str = r#"You are the planning engine for a coding-agent TUI. Reply with JSON ONLY — one object, no extra prose.

Emit the final plan:
{"type":"proposal","goal":"...","success_criteria":"...","verification":["runnable command",...],"rollback":"...","constraints":"...","steps":[{"description":"...","tier":"exec|check|attested|observe","verification":"...","prompt":"...","note":"..."}]}

Verification rules (critical):
- Verification PROVES the step outcome — never replay how you would create files.
- NEVER use as verification: `cat >`, `echo >`, `tee`, `touch`, bare `mkdir` / `mkdir -p`.
- File scaffold → tier `check`, verification `file_exists:<path>` (path relative to project root).
- Directory scaffold → tier `exec`, verification `test -d <path>`.
- Code/content step → tier `check`, verification `grep:<symbol>:<path>`.
- Build/compile → tier `exec`, verification `cmake --build …`, `cargo check`, `make`, etc.
- Run/smoke test → tier `exec` with timeout-safe command (not an infinite game loop).
- `attested` only when no practical check exists — require `note` explaining why.
- `observe` for human-only checks — put question in `prompt`, explain in `note`.

Example steps for a C++ game in subdirectory `galaga/`:
{"description":"Create CMakeLists.txt","tier":"check","verification":"file_exists:CMakeLists.txt"}
{"description":"Implement Player class","tier":"check","verification":"grep:class Player:src/Player.cpp"}
{"description":"Compile project","tier":"exec","verification":"cmake --build build"}

Other rules:
- Keep JSON compact: short strings, max 10 steps, omit optional fields when empty
- Paths in verifications are relative to the project root (e.g. `src/foo.cpp`), not `galaga/src/...` when the project lives in `galaga/`
- Every step needs tier + verification (or `prompt` for observe)
- Include package-install steps if user chose agent install"#;

fn clarify_user_prompt(initial_request: &str, history_text: &str, workspace: &str) -> String {
    format!(
        "Workspace: {workspace}\n\nInitial user request:\n{initial_request}\n\nPrior clarifications:\n{history_text}\n\nEmit the next JSON response."
    )
}

fn proposal_user_prompt(initial_request: &str, history_text: &str, workspace: &str) -> String {
    format!(
        "Workspace: {workspace}\n\nInitial user request:\n{initial_request}\n\nClarifications resolved:\n{history_text}\n\nEmit the full proposal JSON."
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
    let user = clarify_user_prompt(initial_request, &history_text, workspace);
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
    fetch_plan_payload(
        backend,
        PROPOSAL_SYSTEM,
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
    let user = proposal_user_prompt(initial_request, &history_text, workspace);
    let payload = fetch_proposal_payload(backend, &user).await?;
    let PlanModelPayload::Proposal(mut proposal) = payload else {
        return Ok(payload);
    };

    let first_report = improve_proposal(&mut proposal);
    if first_report.errors.is_empty() {
        return Ok(PlanModelPayload::Proposal(proposal));
    }

    let nudge = format_validation_retry_nudge(&first_report.errors);
    let retry_user = format!("{user}{nudge}");
    match fetch_proposal_payload(backend, &retry_user).await {
        Ok(PlanModelPayload::Proposal(mut retry)) => {
            let retry_report = improve_proposal(&mut retry);
            if retry_report.errors.len() <= first_report.errors.len() {
                Ok(PlanModelPayload::Proposal(retry))
            } else {
                Ok(PlanModelPayload::Proposal(proposal))
            }
        }
        Ok(other) => Ok(other),
        Err(_) => Ok(PlanModelPayload::Proposal(proposal)),
    }
}