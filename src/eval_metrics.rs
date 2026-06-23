//! Baseline regression checks against `evals/baselines/metrics.json`.
//! Also writes per-turn harness metrics when `RAVEN_METRICS_OUT` is set.

use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};

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

/// Write agent turn metrics for SWE-bench / harness scorecards (`RAVEN_METRICS_OUT`).
pub fn write_turn_metrics(
    path: &Path,
    result: &TurnResult,
    duration_ms: u64,
    model: &str,
    max_rounds: u32,
) -> Result<()> {
    let tool_counts: serde_json::Map<String, Value> = result
        .actions
        .iter()
        .fold(serde_json::Map::new(), |mut acc, a| {
            let n = acc
                .get(&a.tool)
                .and_then(|v| v.as_u64())
                .unwrap_or(0)
                + 1;
            acc.insert(a.tool.clone(), json!(n));
            acc
        });

    let payload = json!({
        "version": 1,
        "source": "raven_turn",
        "model": model,
        "max_rounds": max_rounds,
        "duration_ms": duration_ms,
        "llm_rounds": result.metrics.llm_rounds,
        "tool_calls": result.metrics.tool_calls,
        "round_limit_hit": result.metrics.round_limit_hit,
        "prompt_tokens": result.metrics.prompt_tokens,
        "completion_tokens": result.metrics.completion_tokens,
        "total_tokens": result.metrics.total_tokens,
        "tool_counts": tool_counts,
        "tools_used": result.actions.iter().map(|a| &a.tool).collect::<Vec<_>>(),
    });

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(&payload)?)?;
    Ok(())
}