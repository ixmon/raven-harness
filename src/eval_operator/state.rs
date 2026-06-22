//! Persistent operator state under `~/.cache/raven-hotel/eval/`.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioResult {
    pub id: String,
    pub passed: bool,
    pub duration_ms: u64,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunSummary {
    pub run_id: String,
    pub profile: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub passed: bool,
    pub results: Vec<ScenarioResult>,
    pub log_dir: PathBuf,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OperatorState {
    pub last_profile: Option<String>,
    pub last_run_id: Option<String>,
    pub selections: Vec<String>,
    pub runs: Vec<RunSummary>,
}

pub fn cache_dir() -> PathBuf {
    dirs_home().join(".cache/raven-hotel/eval")
}

pub fn state_path() -> PathBuf {
    cache_dir().join("state.json")
}

pub fn results_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("evals/results")
}

fn dirs_home() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

pub fn load_state() -> Result<OperatorState> {
    let path = state_path();
    if !path.exists() {
        return Ok(OperatorState::default());
    }
    let data = std::fs::read_to_string(&path)?;
    serde_json::from_str(&data).context("parse eval state.json")
}

pub fn save_state(state: &OperatorState) -> Result<()> {
    let dir = cache_dir();
    std::fs::create_dir_all(&dir)?;
    let data = serde_json::to_string_pretty(state)?;
    std::fs::write(state_path(), data)?;
    Ok(())
}

pub fn push_run(state: &mut OperatorState, summary: RunSummary) {
    state.last_run_id = Some(summary.run_id.clone());
    state.last_profile = Some(summary.profile.clone());
    state.runs.push(summary);
    while state.runs.len() > 32 {
        state.runs.remove(0);
    }
}

pub fn last_run(state: &OperatorState) -> Option<&RunSummary> {
    state.runs.last()
}

pub fn find_run<'a>(state: &'a OperatorState, run_id: &str) -> Option<&'a RunSummary> {
    state.runs.iter().find(|r| r.run_id == run_id)
}

pub fn format_last_run(state: &OperatorState) -> String {
    match last_run(state) {
        None => "Last run: (none)".into(),
        Some(r) => {
            let ok = r.results.iter().filter(|x| x.passed).count();
            let total = r.results.len();
            format!(
                "Last run: {} — {}/{} passed ({})",
                r.profile,
                ok,
                total,
                r.started_at.format("%Y-%m-%d %H:%M")
            )
        }
    }
}