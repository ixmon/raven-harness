//! JSON-driven harness replay scenarios (see `evals/scenarios/`).
//!
//! Run via `cargo test eval_scenarios` or `evals/run_replay.sh`.

use serde::Deserialize;
use serde_json::Value;
use std::path::{Path, PathBuf};

use raven_tui::config::ContextBudget;
use raven_tui::server_probe::{resolve_server_probe, ProbeMatch};

#[derive(Debug, Deserialize)]
struct ProbeExpect {
    model_id: String,
    context_tokens: u32,
}

#[derive(Debug, Deserialize)]
struct ContextBudgetExpect {
    tool_result_bytes: usize,
    read_line_limit: usize,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Scenario {
    Probe {
        name: String,
        body: Value,
        model_hint: String,
        expect: ProbeExpect,
    },
    ContextBudget {
        name: String,
        n_ctx: u32,
        max_rounds: u32,
        expect: ContextBudgetExpect,
    },
}

impl Scenario {
    fn name(&self) -> &str {
        match self {
            Self::Probe { name, .. } | Self::ContextBudget { name, .. } => name,
        }
    }

    fn run(&self) -> Result<(), String> {
        match self {
            Self::Probe {
                body,
                model_hint,
                expect,
                ..
            } => {
                let got = resolve_server_probe(body, model_hint)
                    .ok_or_else(|| format!("probe returned None for hint {:?}", model_hint))?;
                if got.model_id != expect.model_id {
                    return Err(format!(
                        "model_id: got {:?}, want {:?}",
                        got.model_id, expect.model_id
                    ));
                }
                if got.context_tokens != expect.context_tokens {
                    return Err(format!(
                        "context_tokens: got {}, want {}",
                        got.context_tokens, expect.context_tokens
                    ));
                }
                Ok(())
            }
            Self::ContextBudget {
                n_ctx,
                max_rounds,
                expect,
                ..
            } => {
                let b = ContextBudget::from_context_tokens(*n_ctx, *max_rounds);
                if b.tool_result_bytes != expect.tool_result_bytes {
                    return Err(format!(
                        "tool_result_bytes: got {}, want {}",
                        b.tool_result_bytes, expect.tool_result_bytes
                    ));
                }
                if b.read_line_limit != expect.read_line_limit {
                    return Err(format!(
                        "read_line_limit: got {}, want {}",
                        b.read_line_limit, expect.read_line_limit
                    ));
                }
                Ok(())
            }
        }
    }
}

fn scenarios_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("evals/scenarios")
}

pub fn list_scenario_files() -> std::io::Result<Vec<PathBuf>> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(scenarios_dir())?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    paths.sort();
    Ok(paths)
}

fn is_replay_scenario(path: &Path) -> bool {
    let Ok(data) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(raw) = serde_json::from_str::<serde_json::Value>(&data) else {
        return false;
    };
    matches!(
        raw.get("type").and_then(|v| v.as_str()),
        Some("probe") | Some("context_budget")
    )
}

fn load_scenario(path: &Path) -> Result<Scenario, String> {
    let data =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {}", path.display(), e))?;
    serde_json::from_str(&data).map_err(|e| format!("parse {}: {}", path.display(), e))
}

/// Run every `evals/scenarios/*.json` file. Returns list of `(name, error)`.
pub fn run_all_scenarios() -> Vec<(String, String)> {
    let Ok(files) = list_scenario_files() else {
        return vec![(
            "(scenarios dir)".into(),
            "could not read evals/scenarios".into(),
        )];
    };

    let mut failures = vec![];
    for path in files {
        if !is_replay_scenario(&path) {
            continue;
        }
        let scenario = match load_scenario(&path) {
            Ok(s) => s,
            Err(e) => {
                failures.push((path.display().to_string(), e));
                continue;
            }
        };
        let name = scenario.name().to_string();
        if let Err(e) = scenario.run() {
            failures.push((name, e));
        }
    }
    failures
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eval_scenarios_all_pass() {
        let failures = run_all_scenarios();
        if failures.is_empty() {
            return;
        }
        let msg: Vec<String> = failures
            .iter()
            .map(|(n, e)| format!("  FAIL {n}: {e}"))
            .collect();
        panic!("harness replay scenarios failed:\n{}", msg.join("\n"));
    }

    #[test]
    fn probe_single_model_fallback_uses_single_model_match() {
        let path = scenarios_dir().join("probe_single_model_fallback.json");
        let scenario = load_scenario(&path).expect("load scenario");
        let Scenario::Probe {
            body, model_hint, ..
        } = scenario
        else {
            panic!("wrong scenario type");
        };
        let r = resolve_server_probe(&body, &model_hint).expect("resolve");
        assert_eq!(r.matched_by, ProbeMatch::SingleModel);
    }
}
