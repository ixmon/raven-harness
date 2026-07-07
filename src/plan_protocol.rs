//! JSON protocol for harness-driven plan mode (clarify → propose → proceed).

use serde::{Deserialize, Serialize};

/// One multiple-choice option in a clarification question.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanQuestionOption {
    pub id: String,
    pub label: String,
}

/// Clarification question emitted by the model.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanQuestion {
    pub id: String,
    pub prompt: String,
    /// `choice` or `text`
    #[serde(default = "default_choice_kind")]
    pub kind: String,
    #[serde(default)]
    pub options: Vec<PlanQuestionOption>,
    #[serde(default)]
    pub recommend: Option<String>,
    #[serde(default = "default_true")]
    pub allow_free_text: bool,
}

fn default_choice_kind() -> String {
    "choice".to_string()
}

fn default_true() -> bool {
    true
}

/// Step in a final plan proposal.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanProposalStep {
    pub description: String,
    #[serde(default)]
    pub tier: Option<String>,
    #[serde(default)]
    pub verification: Option<String>,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

/// Full plan proposal after clarification is complete.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanProposal {
    pub goal: String,
    pub success_criteria: String,
    #[serde(default)]
    pub verification: Vec<String>,
    #[serde(default)]
    pub rollback: Option<String>,
    #[serde(default)]
    pub constraints: Option<String>,
    #[serde(default)]
    pub steps: Vec<PlanProposalStep>,
}

/// Model JSON payload for one plan-loop turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanModelPayload {
    Clarify { question: PlanQuestion },
    Ready { message: Option<String> },
    Proposal(PlanProposal),
}

#[derive(Debug, Deserialize)]
struct PlanModelResponseRaw {
    #[serde(rename = "type")]
    response_type: String,
    question: Option<PlanQuestion>,
    message: Option<String>,
    goal: Option<String>,
    success_criteria: Option<String>,
    verification: Option<Vec<String>>,
    rollback: Option<String>,
    constraints: Option<String>,
    steps: Option<Vec<PlanProposalStep>>,
}

/// Record of one clarify Q&A round (fed back into the next model call).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanQaEntry {
    pub question_id: String,
    pub question_prompt: String,
    pub user_input: String,
    pub resolution: String,
}

fn strip_thinking_tags(text: &str) -> String {
    let mut out = text.to_string();
    while let Some(start) = out.find("<think>") {
        if let Some(rel_end) = out[start..].find("</think>") {
            let end = start + rel_end + "</think>".len();
            out.replace_range(start..end, "");
        } else {
            out.replace_range(start..start + "<think>".len(), "");
        }
    }
    out
}

fn extract_fenced_json(text: &str) -> Option<String> {
    for marker in ["```json", "```JSON", "```"] {
        let mut search = 0usize;
        while let Some(rel) = text[search..].find(marker) {
            let after_marker = search + rel + marker.len();
            let rest = text[after_marker..].trim_start();
            if let Some(close) = rest.find("```") {
                let body = rest[..close].trim();
                if body.starts_with('{') {
                    return Some(body.to_string());
                }
            }
            search = after_marker;
        }
    }
    None
}

fn extract_braced_json(text: &str) -> Option<String> {
    let start = text.find('{')?;
    let mut depth = 0i32;
    let mut in_str = false;
    let mut escape = false;
    for (i, ch) in text[start..].char_indices() {
        if in_str {
            if escape {
                escape = false;
            } else if ch == '\\' {
                escape = true;
            } else if ch == '"' {
                in_str = false;
            }
            continue;
        }
        match ch {
            '"' => in_str = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(text[start..=start + i].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

/// Extract the first JSON object from model output (strips markdown fences and thinking tags).
pub fn extract_json_object(text: &str) -> Option<String> {
    let cleaned = strip_thinking_tags(text.trim());
    if let Some(json) = extract_fenced_json(&cleaned) {
        return Some(json);
    }
    extract_braced_json(&cleaned)
}

/// Pick the best assistant text slice for plan JSON parsing (content vs reasoning).
pub fn text_for_plan_json_parse(content: &str, reasoning: Option<&str>) -> String {
    for candidate in [content, reasoning.unwrap_or("")] {
        let trimmed = candidate.trim();
        if !trimmed.is_empty() && extract_json_object(trimmed).is_some() {
            return trimmed.to_string();
        }
    }
    match (content.trim(), reasoning.map(str::trim).filter(|s| !s.is_empty())) {
        ("", Some(r)) => r.to_string(),
        (c, Some(r)) => format!("{c}\n{r}"),
        (c, None) => c.to_string(),
    }
}

/// True when output starts like JSON but lacks a closing brace (typical of token-limit truncation).
pub fn looks_truncated_json(text: &str) -> bool {
    let cleaned = strip_thinking_tags(text.trim());
    cleaned.contains('{') && extract_braced_json(&cleaned).is_none()
}

pub fn parse_plan_model_payload(text: &str) -> Result<PlanModelPayload, String> {
    let json = extract_json_object(text).ok_or_else(|| {
        let snippet: String = text.chars().take(240).collect();
        if looks_truncated_json(text) {
            format!(
                "truncated JSON in model response (likely hit token limit; snippet: {snippet:?})"
            )
        } else {
            format!("no JSON object in model response (snippet: {snippet:?})")
        }
    })?;
    let raw: PlanModelResponseRaw =
        serde_json::from_str(&json).map_err(|e| format!("invalid plan JSON: {e}"))?;
    match raw.response_type.as_str() {
        "clarify" => {
            let question = raw
                .question
                .ok_or_else(|| "clarify response missing question".to_string())?;
            if question.id.trim().is_empty() || question.prompt.trim().is_empty() {
                return Err("question requires non-empty id and prompt".to_string());
            }
            Ok(PlanModelPayload::Clarify { question })
        }
        "ready" => Ok(PlanModelPayload::Ready {
            message: raw.message,
        }),
        "proposal" => {
            let goal = raw.goal.filter(|s| !s.trim().is_empty())
                .ok_or_else(|| "proposal missing goal".to_string())?;
            let success_criteria = raw
                .success_criteria
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| "proposal missing success_criteria".to_string())?;
            Ok(PlanModelPayload::Proposal(PlanProposal {
                goal,
                success_criteria,
                verification: raw.verification.unwrap_or_default(),
                rollback: raw.rollback,
                constraints: raw.constraints,
                steps: raw.steps.unwrap_or_default(),
            }))
        }
        other => Err(format!("unknown plan response type: {other}")),
    }
}

/// Human-readable block for the conversation pane.
pub fn format_question_for_user(q: &PlanQuestion) -> String {
    let mut lines = vec![format!("❓ {}", q.prompt)];
    if !q.options.is_empty() {
        for (i, opt) in q.options.iter().enumerate() {
            let rec = q
                .recommend
                .as_deref()
                .is_some_and(|r| r == opt.id)
                .then_some(" ← recommended");
            lines.push(format!(
                "  {}. {}{}",
                i + 1,
                opt.label,
                rec.unwrap_or_default()
            ));
        }
        if q.allow_free_text {
            lines.push("  (or type your own answer)".to_string());
        }
    } else if q.kind == "text" {
        lines.push("  (type your answer)".to_string());
    }
    lines.join("\n")
}

pub fn format_proposal_for_user(p: &PlanProposal) -> String {
    let mut lines = vec![
        "📋 Proposed plan".to_string(),
        format!("**Goal:** {}", p.goal),
        format!("**Success criteria:** {}", p.success_criteria),
    ];
    if !p.verification.is_empty() {
        lines.push("**Verification:**".to_string());
        for v in &p.verification {
            lines.push(format!("  • {v}"));
        }
    }
    if let Some(r) = &p.rollback {
        if !r.trim().is_empty() {
            lines.push(format!("**Rollback:** {r}"));
        }
    }
    if !p.steps.is_empty() {
        lines.push("**Steps:**".to_string());
        for (i, st) in p.steps.iter().enumerate() {
            let tier = st.tier.as_deref().unwrap_or("exec");
            let verify = st
                .verification
                .as_deref()
                .or(st.prompt.as_deref())
                .unwrap_or("");
            lines.push(format!("  {}. {} [{}: {verify}]", i + 1, st.description, tier));
        }
    }
    lines.push(String::new());
    lines.push("Ready to proceed? (yes / proceed / or suggest changes)".to_string());
    lines.join("\n")
}

pub fn format_qa_history_for_prompt(history: &[PlanQaEntry]) -> String {
    if history.is_empty() {
        return "(no prior clarifications)".to_string();
    }
    history
        .iter()
        .map(|e| {
            format!(
                "- Q [{}]: {}\n  User said: {}\n  Resolved as: {}",
                e.question_id, e.question_prompt, e.user_input, e.resolution
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_clarify_json() {
        let raw = r#"```json
{"type":"clarify","question":{"id":"loc","prompt":"Where?","options":[{"id":"a","label":"Here"},{"id":"b","label":"There"}],"recommend":"a"}}
```"#;
        let p = parse_plan_model_payload(raw).unwrap();
        match p {
            PlanModelPayload::Clarify { question } => {
                assert_eq!(question.id, "loc");
                assert_eq!(question.options.len(), 2);
                assert_eq!(question.recommend.as_deref(), Some("a"));
            }
            _ => panic!("expected clarify"),
        }
    }

    #[test]
    fn parses_proposal_json() {
        let raw = r#"{"type":"proposal","goal":"Ship it","success_criteria":"Tests pass","verification":["cargo test"],"steps":[{"description":"Implement","tier":"exec","verification":"cargo test"}]}"#;
        let p = parse_plan_model_payload(raw).unwrap();
        match p {
            PlanModelPayload::Proposal(prop) => {
                assert_eq!(prop.goal, "Ship it");
                assert_eq!(prop.steps.len(), 1);
            }
            _ => panic!("expected proposal"),
        }
    }

    #[test]
    fn detects_truncated_json() {
        let partial = r#"{"type":"proposal","goal":"Build a game with many features including "#;
        assert!(looks_truncated_json(partial));
        assert!(parse_plan_model_payload(partial).unwrap_err().contains("truncated"));
    }

    #[test]
    fn parses_json_after_prose_and_fence() {
        let raw = r#"Sure, here is the question:
```json
{"type":"clarify","question":{"id":"loc","prompt":"Where?","options":[],"kind":"text"}}
```"#;
        let p = parse_plan_model_payload(raw).unwrap();
        assert!(matches!(p, PlanModelPayload::Clarify { .. }));
    }

    #[test]
    fn text_for_plan_json_parse_prefers_reasoning_with_json() {
        let content = "Let me think about this...";
        let reasoning = r#"{"type":"ready","message":"done"}"#;
        let text = text_for_plan_json_parse(content, Some(reasoning));
        let p = parse_plan_model_payload(&text).unwrap();
        assert!(matches!(p, PlanModelPayload::Ready { .. }));
    }

    #[test]
    fn format_question_shows_recommendation() {
        let q = PlanQuestion {
            id: "x".into(),
            prompt: "Pick".into(),
            kind: "choice".into(),
            options: vec![
                PlanQuestionOption {
                    id: "a".into(),
                    label: "Alpha".into(),
                },
                PlanQuestionOption {
                    id: "b".into(),
                    label: "Beta".into(),
                },
            ],
            recommend: Some("b".into()),
            allow_free_text: true,
        };
        let text = format_question_for_user(&q);
        assert!(text.contains("← recommended"));
        assert!(text.contains("Beta"));
    }
}