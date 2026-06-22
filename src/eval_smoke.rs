//! Live LLM smoke scenarios (`RAVEN_EVAL=1`). Replay scenarios stay in `eval_scenarios.rs`.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::Value;

use crate::agent::TurnResult;
use crate::session::Session;
use crate::chat_backend::{mock_tool_call, MockChatBackend};
use crate::llm::ChatResponse;
use crate::tools::backend::MockToolBackend;

#[derive(Debug, Default, Deserialize)]
pub struct MockLlmToolCall {
    pub name: String,
    #[serde(default)]
    pub arguments: Value,
}

#[derive(Debug, Default, Deserialize)]
pub struct MockLlmTurn {
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub tool_calls: Vec<MockLlmToolCall>,
}

#[derive(Debug, Default, Deserialize)]
pub struct SmokeExpect {
    #[serde(default)]
    pub stdout_contains: Vec<String>,
    #[serde(default)]
    pub max_tool_rounds: Option<u32>,
    #[serde(default)]
    pub min_tool_calls: Option<u32>,
    #[serde(default)]
    pub tools_used: Vec<String>,
    #[serde(default)]
    pub any_action_truncated: Option<bool>,
    #[serde(default)]
    pub output_must_not_contain: Vec<String>,
    #[serde(default)]
    pub log_must_not_contain: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct SmokeScenario {
    pub name: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub description: String,
    pub prompt: String,
    #[serde(default)]
    pub mock_tools: Value,
    #[serde(default)]
    pub llm_turns: Vec<MockLlmTurn>,
    /// Override context window for budget-sensitive scenarios.
    #[serde(default)]
    pub context_tokens: Option<u32>,
    #[serde(default)]
    pub max_rounds: Option<u32>,
    #[serde(default)]
    pub expect: SmokeExpect,
}

/// Offline smoke scenarios that ship scripted `llm_turns`.
#[allow(dead_code)]
pub fn list_offline_smoke_scenarios() -> Result<Vec<String>> {
    let mut names = vec![];
    for path in std::fs::read_dir(scenarios_dir())? {
        let path = path?.path();
        if path.extension().is_some_and(|x| x == "json") {
            let data = std::fs::read_to_string(&path)?;
            let raw: Value = serde_json::from_str(&data)?;
            if raw.get("type").and_then(|v| v.as_str()) == Some("smoke")
                && raw.get("llm_turns")
                    .and_then(|v| v.as_array())
                    .is_some_and(|a| !a.is_empty())
            {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    names.push(stem.to_string());
                }
            }
        }
    }
    names.sort();
    Ok(names)
}

pub fn mock_llm_enabled() -> bool {
    std::env::var("RAVEN_EVAL_MOCK_LLM")
        .ok()
        .is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
}

pub fn eval_enabled() -> bool {
    std::env::var("RAVEN_EVAL")
        .ok()
        .is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
}

pub fn scenario_name() -> String {
    std::env::var("RAVEN_EVAL_SCENARIO").unwrap_or_else(|_| "smoke_ping".into())
}

pub fn scenarios_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("evals/scenarios")
}

pub fn default_eval_workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("evals/fixtures/empty")
}

pub fn resolve_eval_workspace() -> Result<PathBuf> {
    if let Ok(ws) = std::env::var("RAVEN_EVAL_WORKSPACE") {
        let p = PathBuf::from(ws);
        std::fs::create_dir_all(&p)?;
        return Ok(p);
    }
    let p = default_eval_workspace();
    std::fs::create_dir_all(&p)?;
    Ok(p)
}

pub fn load_smoke_scenario(name: &str) -> Result<SmokeScenario> {
    let path = scenarios_dir().join(format!("{name}.json"));
    let data = std::fs::read_to_string(&path)
        .with_context(|| format!("read smoke scenario {}", path.display()))?;
    let raw: Value = serde_json::from_str(&data)
        .with_context(|| format!("parse smoke scenario {}", path.display()))?;
    let ty = raw.get("type").and_then(|v| v.as_str()).unwrap_or("");
    if ty != "smoke" {
        bail!(
            "scenario {} has type {:?}, expected \"smoke\"",
            path.display(),
            ty
        );
    }
    serde_json::from_value(raw).context("deserialize smoke scenario")
}

pub fn mock_backend_for(scenario: &SmokeScenario) -> MockToolBackend {
    MockToolBackend::from_json(&scenario.mock_tools)
}

pub fn mock_chat_backend_for(scenario: &SmokeScenario) -> MockChatBackend {
    let turns: Vec<ChatResponse> = scenario
        .llm_turns
        .iter()
        .map(|t| {
            let tool_calls: Vec<_> = t
                .tool_calls
                .iter()
                .map(|tc| {
                    let args = if tc.arguments.is_null() {
                        "{}".to_string()
                    } else {
                        tc.arguments.to_string()
                    };
                    mock_tool_call(&tc.name, &args)
                })
                .collect();
            let finish = if tool_calls.is_empty() {
                Some("stop".into())
            } else {
                Some("tool_calls".into())
            };
            ChatResponse {
                content: t.content.clone(),
                tool_calls,
                finish_reason: finish,
                usage: None,
            }
        })
        .collect();
    MockChatBackend::new(turns)
}

pub fn assert_smoke_result(scenario: &SmokeScenario, result: &TurnResult) -> Result<()> {
    let text = result.final_text.to_lowercase();
    for needle in &scenario.expect.stdout_contains {
        if !text.contains(&needle.to_lowercase()) {
            bail!(
                "smoke {:?}: stdout missing {:?}",
                scenario.name,
                needle
            );
        }
    }

    if let Some(max) = scenario.expect.max_tool_rounds {
        let tool_calls = result.actions.len() as u32;
        if tool_calls > max {
            bail!(
                "smoke {:?}: used {} tool calls, max {}",
                scenario.name,
                tool_calls,
                max
            );
        }
    }

    if let Some(min) = scenario.expect.min_tool_calls {
        let tool_calls = result.actions.len() as u32;
        if tool_calls < min {
            bail!(
                "smoke {:?}: used {} tool calls, min {}",
                scenario.name,
                tool_calls,
                min
            );
        }
    }

    if !scenario.expect.tools_used.is_empty() {
        let used: Vec<&str> = result.actions.iter().map(|a| a.tool.as_str()).collect();
        for want in &scenario.expect.tools_used {
            if !used.iter().any(|u| *u == want) {
                bail!(
                    "smoke {:?}: expected tool {:?} in {:?}",
                    scenario.name,
                    want,
                    used
                );
            }
        }
    }

    if let Some(want_truncated) = scenario.expect.any_action_truncated {
        let any = result.actions.iter().any(|a| a.truncated);
        if any != want_truncated {
            bail!(
                "smoke {:?}: any_action_truncated got {}, want {}",
                scenario.name,
                any,
                want_truncated
            );
        }
    }

    for needle in &scenario.expect.output_must_not_contain {
        for action in &result.actions {
            if action.output_to_model.contains(needle) {
                bail!(
                    "smoke {:?}: tool output to model contains forbidden {:?}",
                    scenario.name,
                    needle
                );
            }
        }
    }

    Ok(())
}

pub fn assert_smoke_session_log(scenario: &SmokeScenario, workspace: &Path) -> Result<()> {
    if scenario.expect.log_must_not_contain.is_empty() {
        return Ok(());
    }
    let session = Session::init(workspace)?;
    let log = std::fs::read_to_string(&session.log_path)
        .with_context(|| format!("read {}", session.log_path.display()))?;
    for needle in &scenario.expect.log_must_not_contain {
        if log.contains(needle) {
            bail!(
                "smoke {:?}: full_log.jsonl contains forbidden {:?}",
                scenario.name,
                needle
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_smoke_ping_scenario() {
        let s = load_smoke_scenario("smoke_ping").expect("load");
        assert_eq!(s.name, "smoke_ping");
        assert!(!s.prompt.is_empty());
    }

    #[test]
    fn mock_tool_loop_has_llm_script() {
        let s = load_smoke_scenario("mock_tool_loop").expect("load");
        assert_eq!(s.llm_turns.len(), 2);
        assert_eq!(s.llm_turns[0].tool_calls[0].name, "list");
    }

    #[test]
    fn list_offline_smoke_includes_new_scenarios() {
        let names = list_offline_smoke_scenarios().expect("list");
        for want in [
            "mock_tool_loop",
            "mock_churn_then_answer",
            "mock_huge_grep",
            "mock_secrets_in_read",
        ] {
            assert!(names.iter().any(|n| n == want), "missing {want}");
        }
    }

    #[test]
    fn assert_smoke_ping_passes_on_marker() {
        let s = load_smoke_scenario("smoke_ping").expect("load");
        let result = TurnResult {
            final_text: "SMOKE_OK — all good".into(),
            actions: vec![],
            rounds_used: 0,
        };
        assert_smoke_result(&s, &result).expect("assert");
    }
}