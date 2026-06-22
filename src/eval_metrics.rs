//! Baseline regression checks against `evals/baselines/metrics.json`.

use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::Value;

use crate::agent::TurnResult;
use crate::eval_smoke::SmokeScenario;

#[derive(Debug, Deserialize)]
struct MetricsFile {
    smoke: Option<Value>,
}

pub fn strict_assertions_enabled() -> bool {
    std::env::var("RAVEN_EVAL_ASSERT_STRICT")
        .ok()
        .is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
}

fn baselines_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("evals/baselines/metrics.json")
}

pub fn assert_smoke_metrics(scenario: &SmokeScenario, result: &TurnResult) -> Result<()> {
    if !strict_assertions_enabled() {
        return Ok(());
    }

    let data = std::fs::read_to_string(baselines_path())
        .with_context(|| format!("read {}", baselines_path().display()))?;
    let metrics: MetricsFile =
        serde_json::from_str(&data).context("parse evals/baselines/metrics.json")?;

    let Some(smoke) = metrics.smoke else {
        return Ok(());
    };
    let Some(entry) = smoke.get(&scenario.name) else {
        return Ok(());
    };

    if let Some(want) = entry.get("tool_calls").and_then(|v| v.as_u64()) {
        let got = result.actions.len() as u64;
        if got != want {
            bail!(
                "metrics strict {:?}: tool_calls got {}, want {}",
                scenario.name,
                got,
                want
            );
        }
    }

    if let Some(want) = entry.get("max_tool_rounds").and_then(|v| v.as_u64()) {
        let got = result.actions.len() as u64;
        if got > want {
            bail!(
                "metrics strict {:?}: tool_calls {} exceeds baseline max {}",
                scenario.name,
                got,
                want
            );
        }
    }

    Ok(())
}