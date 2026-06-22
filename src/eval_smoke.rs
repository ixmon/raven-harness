//! Live LLM smoke scenarios (`RAVEN_EVAL=1`). Replay scenarios stay in `eval_scenarios.rs`.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::Value;

use crate::agent::TurnResult;
use crate::tools::backend::MockToolBackend;

#[derive(Debug, Default, Deserialize)]
pub struct SmokeExpect {
    #[serde(default)]
    pub stdout_contains: Vec<String>,
    #[serde(default)]
    pub max_tool_rounds: Option<u32>,
    #[serde(default)]
    pub tools_used: Vec<String>,
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
    pub expect: SmokeExpect,
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