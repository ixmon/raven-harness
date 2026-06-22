//! Discover harness tests from `evals/scenarios/` and fixed tiers.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TestTier {
    Replay,
    MockSmoke,
    LiveSmoke,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestEntry {
    pub id: String,
    pub tier: TestTier,
    pub description: String,
    pub needs_llm: bool,
    pub offline: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Registry {
    pub fixed: Vec<TestEntry>,
    pub scenarios: Vec<TestEntry>,
}

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

pub fn scenarios_dir() -> PathBuf {
    manifest_dir().join("evals/scenarios")
}

pub fn load_registry() -> Result<Registry> {
    let fixed = vec![
        TestEntry {
            id: "replay".into(),
            tier: TestTier::Replay,
            description: "Offline probe + context_budget JSON scenarios".into(),
            needs_llm: false,
            offline: true,
        },
        TestEntry {
            id: "mock_smoke_all".into(),
            tier: TestTier::MockSmoke,
            description: "All offline mock LLM + mock tool scenarios".into(),
            needs_llm: false,
            offline: true,
        },
        TestEntry {
            id: "unit_tests".into(),
            tier: TestTier::Replay,
            description: "Full cargo test --no-default-features".into(),
            needs_llm: false,
            offline: true,
        },
        TestEntry {
            id: "swebench-smoke".into(),
            tier: TestTier::Replay,
            description: "SWE-bench Lite dev smoke trio (verify-grade; needs uv)".into(),
            needs_llm: false,
            offline: true,
        },
    ];

    let mut scenarios = vec![];
    let dir = scenarios_dir();
    if dir.is_dir() {
        let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .collect();
        paths.sort();
        for path in paths {
            if let Some(entry) = load_scenario_entry(&path)? {
                scenarios.push(entry);
            }
        }
    }

    Ok(Registry { fixed, scenarios })
}

fn load_scenario_entry(path: &Path) -> Result<Option<TestEntry>> {
    let data = std::fs::read_to_string(path)
        .with_context(|| format!("read {}", path.display()))?;
    let raw: Value = serde_json::from_str(&data)?;
    let ty = raw.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let id = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("?")
        .to_string();
    let description = raw
        .get("description")
        .or_else(|| raw.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or(&id)
        .to_string();

    let entry = match ty {
        "probe" | "context_budget" => Some(TestEntry {
            id,
            tier: TestTier::Replay,
            description,
            needs_llm: false,
            offline: true,
        }),
        "smoke" => {
            let has_mock_llm = raw
                .get("llm_turns")
                .and_then(|v| v.as_array())
                .is_some_and(|a| !a.is_empty());
            let scenario_id = raw
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or(&id)
                .to_string();
            Some(TestEntry {
                id: scenario_id,
                tier: if has_mock_llm {
                    TestTier::MockSmoke
                } else {
                    TestTier::LiveSmoke
                },
                description,
                needs_llm: !has_mock_llm,
                offline: has_mock_llm,
            })
        }
        _ => None,
    };
    Ok(entry)
}

pub fn list_text(reg: &Registry) -> String {
    let mut out = String::from("Fixed tiers:\n");
    for t in &reg.fixed {
        out.push_str(&format!(
            "  {} [{}] — {}\n",
            t.id,
            tier_label(t.tier),
            t.description
        ));
    }
    out.push_str("\nScenarios:\n");
    for t in &reg.scenarios {
        out.push_str(&format!(
            "  {} [{}]{} — {}\n",
            t.id,
            tier_label(t.tier),
            if t.offline { " offline" } else { " live" },
            t.description
        ));
    }
    out
}

fn tier_label(t: TestTier) -> &'static str {
    match t {
        TestTier::Replay => "replay",
        TestTier::MockSmoke => "mock",
        TestTier::LiveSmoke => "live",
    }
}

pub fn find_entry<'a>(reg: &'a Registry, id: &str) -> Option<&'a TestEntry> {
    reg.fixed
        .iter()
        .find(|e| e.id == id)
        .or_else(|| reg.scenarios.iter().find(|e| e.id == id))
}

pub fn profile_ids(profile: &str) -> Result<Vec<String>> {
    let reg = load_registry()?;
    let ids = match profile {
        "quick" => vec!["replay".into(), "mock_smoke_all".into()],
        "local" => vec!["replay".into(), "mock_smoke_all".into(), "smoke_ping".into()],
        "full" => {
            let mut ids = vec!["replay".into(), "mock_smoke_all".into()];
            for s in &reg.scenarios {
                if s.tier == TestTier::LiveSmoke {
                    ids.push(s.id.clone());
                }
            }
            ids
        }
        "swebench-smoke" => super::swebench::smoke_trio_ids(&manifest_dir())?,
        other => anyhow::bail!("unknown profile {other:?} (use quick, local, full, or swebench-smoke)"),
    };
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_loads_scenarios() {
        let reg = load_registry().expect("load");
        assert!(!reg.fixed.is_empty());
        assert!(reg.scenarios.iter().any(|s| s.id == "smoke_ping"));
        assert!(reg.scenarios.iter().any(|s| s.id == "mock_tool_loop"));
    }

    #[test]
    fn swebench_smoke_profile_has_trio() {
        let ids = profile_ids("swebench-smoke").expect("profile");
        assert_eq!(ids.len(), 3);
    }
}