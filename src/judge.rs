// Judge module - handles inference-based judgment of agent progress
use std::sync::Arc;
use tokio::sync::Mutex;
use crate::agent::{TurnJudge, ActionRecord};
use crate::chat_backend::ChatBackend;
use crate::session::SessionMeta;

pub struct Judge {
    client: Arc<Mutex<ChatBackend>>,
}

impl Judge {
    pub fn new(client: Arc<Mutex<ChatBackend>>) -> Self {
        Self { client }
    }

    /// Judge whether the agent's recent actions satisfy the task criteria.
    /// This is the core inference-based judge that calls an LLM.
    pub async fn judge_turn(
        &self,
        meta: &SessionMeta,
        last_assistant_text: &str,
        recent_actions: &[ActionRecord],
    ) -> TurnJudge {
        const JUDGE_MAX_TOKENS: u32 = 256;

        if (meta.current_goal.trim().is_empty() || meta.current_goal.contains("not yet established"))
            && meta.completion_criteria.as_ref().is_none_or(|c| c.trim().is_empty())
        {
            return TurnJudge::Continue { suggestion: None };
        }

        // Build compact recent activity for loop detection
        let activity: String = recent_actions
            .iter()
            .rev()
            .take(6)
            .map(|a| {
                if a.tool == "exec" {
                    let lines: Vec<&str> = a.output_to_model.lines().take(48).collect();
                    let preview = if lines.is_empty() {
                        "(no output)".to_string()
                    } else {
                        lines.join("\n")
                    };
                    format!("• {} →\n{}", a.tool, preview)
                } else {
                    format!("• {} → {}", a.tool, a.summary)
                }
            })
            .collect::<Vec<_>>()
            .join("\n");

        let mut judge_prompt = format!(
            "Current goal: {}\n\n",
            meta.current_goal
        );

        if !meta.achievement_tests.is_empty() {
            judge_prompt.push_str("Success criteria (only answer FULFILLED if these are clearly met):\n");
            for test in &meta.achievement_tests {
                judge_prompt.push_str(&format!("- {}\n", test));
            }
            judge_prompt.push('\n');
        }

        if let Some(criteria) = &meta.completion_criteria {
            judge_prompt.push_str("Agent-defined 'done' definition (what completion looks like):\n");
            judge_prompt.push_str(criteria);
            judge_prompt.push_str("\n(Answer FULFILLED only if recent actions clearly satisfy this definition.)\n\n");
        }

        // Explicitly include the original user request so the judge knows the full intent
        if let Some(req) = &meta.last_user_request {
            judge_prompt.push_str(&format!("Original user request:\n{}\n\n", req));
        }

        if !activity.is_empty() {
            judge_prompt.push_str("Recent actions (most recent last):\n");
            judge_prompt.push_str(&activity);
            judge_prompt.push_str("\n\n");
        }

        judge_prompt.push_str(&format!(
            "Latest model output:\n{}\n\n\
             CRITICAL RULES FOR DECISION:\n\
             - Only answer FULFILLED if the actions provide clear evidence that the ENTIRE request was completed (not just the model's claim).\n\
             - If the request asked to 'write AND run AND show output', there must be an 'exec' action with matching output in the recent actions.\n\
             - A write alone is never enough for a 'run it' request.\n\
             - For bug report / code fix requests (no explicit 'run' language): FULFILLED requires at least one successful `write` or `patch` action on a main source file in the library (e.g. under src/, the package dir), not merely on a temp diagnostic script the agent created. The edit must address the reported issue.\n\
             - If the agent defined a completion_criteria via define_done (ideally derived from the *initial* user request), FULFILLED only if actions clearly satisfy that exact definition. The definition should have been set early from the first message.\n\
             - If the definition requires running/showing/printing/verifying output or proof, FULFILLED requires visible evidence of that (recent exec whose stdout is shown).\n\
             - If the model is claiming success without evidence in actions, treat as not fulfilled.\n\n\
             Reply format (first line exactly one of):\n\
             FULFILLED\n\
             <short note>\n\
             or\n\
             CONTINUE\n\
             <short reason why not fulfilled yet>\n\
             <one specific actionable suggestion: what the agent should do RIGHT NOW to satisfy the definition (e.g. \"run the script with exec and paste the exact output here so the proof is visible to the judge\")>\n\
             or\n\
             STUCK\n\
             <reason>\n\
             <specific question the agent should ask the user>",
            last_assistant_text.trim()
        ));

        let req = crate::llm::ChatRequest {
            messages: vec![crate::llm::Message {
                role: "user".into(),
                content: Some(judge_prompt),
                tool_calls: None,
                tool_call_id: None,
            }],
            tools: None,
            temperature: 0.0,
            max_tokens: JUDGE_MAX_TOKENS,
            stream: false,
        };

        let response = match self.client.lock().await.chat(req).await {
            Ok(r) => r.content.trim().to_string(),
            Err(_) => return TurnJudge::Continue { suggestion: None },
        };

        let upper = response.to_uppercase();
        let lines: Vec<&str> = response.lines().collect();

        if upper.contains("FULFILLED") {
            TurnJudge::Fulfilled { note: response }
        } else if upper.contains("STUCK") {
            let reason = lines.get(1).unwrap_or(&"Repeating similar actions without progress").to_string();
            let suggested = lines.get(2).unwrap_or(&"What additional information or direction do you have?").to_string();
            TurnJudge::Stuck { reason, suggested_guidance: suggested.to_string() }
        } else {
            let suggestion = if lines.len() > 1 {
                let rest = lines[1..].join(" ").trim().to_string();
                if rest.is_empty() { None } else { Some(rest) }
            } else { None };
            TurnJudge::Continue { suggestion }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat_backend::MockChatBackend;
    use crate::llm::ChatResponse;
    use crate::session::SessionMeta;

    fn make_test_meta() -> SessionMeta {
        SessionMeta {
            session_id: "test-session".to_string(),
            workspace: std::path::PathBuf::from("/tmp/test"),
            trusted: true,
            created_at: chrono::Utc::now().to_rfc3339(),
            updated_at: chrono::Utc::now().to_rfc3339(),
            current_goal: "Test goal".to_string(),
            achievement_tests: vec![],
            completion_criteria: None,
            pitfalls: vec![],
            discoveries: vec![],
            last_user_request: None,
            repo_cache: crate::session::RepoCache::default(),
            recent_turns_summary: String::new(),
            exec_approval_mode: crate::session::ExecApprovalMode::default(),
            initial_analysis: None,
            last_judge: None,
        }
    }

    #[test]
    fn test_build_judge_prompt_basic() {
        let meta = make_test_meta();
        let recent_actions: Vec<crate::agent::ActionRecord> = vec![];
        let last_assistant_text = "test output";

        // This is a placeholder test - the actual prompt construction
        // is tested indirectly through integration tests
        let _ = meta;
        let _ = recent_actions;
        let _ = last_assistant_text;
    }

    #[tokio::test]
    async fn test_parse_judge_fulfilled() {
        let mock = MockChatBackend::new(vec![
            ChatResponse {
                content: "FULFILLED\nAll tests pass".to_string(),
                tool_calls: vec![],
                finish_reason: Some("stop".to_string()),
                usage: None,
            }
        ]);
        let judge = Judge::new(Arc::new(Mutex::new(ChatBackend::Mock(mock))));
        
        let meta = make_test_meta();
        let result = judge.judge_turn(&meta, "test", &[]).await;
        assert!(matches!(result, TurnJudge::Fulfilled { .. }));
    }

    #[tokio::test]
    async fn test_parse_judge_continue() {
        let mock = MockChatBackend::new(vec![
            ChatResponse {
                content: "CONTINUE\nStill working\nRun the script to see output".to_string(),
                tool_calls: vec![],
                finish_reason: Some("stop".to_string()),
                usage: None,
            }
        ]);
        let judge = Judge::new(Arc::new(Mutex::new(ChatBackend::Mock(mock))));
        
        let meta = make_test_meta();
        let result = judge.judge_turn(&meta, "test", &[]).await;
        assert!(matches!(result, TurnJudge::Continue { .. }));
    }

    #[tokio::test]
    async fn test_parse_judge_stuck() {
        let mock = MockChatBackend::new(vec![
            ChatResponse {
                content: "STUCK\nRepeating same action\nAsk user for guidance".to_string(),
                tool_calls: vec![],
                finish_reason: Some("stop".to_string()),
                usage: None,
            }
        ]);
        let judge = Judge::new(Arc::new(Mutex::new(ChatBackend::Mock(mock))));
        
        let meta = make_test_meta();
        let result = judge.judge_turn(&meta, "test", &[]).await;
        assert!(matches!(result, TurnJudge::Stuck { .. }));
    }

    #[tokio::test]
    async fn test_judge_no_criteria() {
        let mock = MockChatBackend::new(vec![]);
        let judge = Judge::new(Arc::new(Mutex::new(ChatBackend::Mock(mock))));
        
        let mut meta = make_test_meta();
        meta.current_goal = "not yet established".to_string();
        let result = judge.judge_turn(&meta, "test", &[]).await;
        assert!(matches!(result, TurnJudge::Continue { .. }));
    }
}
