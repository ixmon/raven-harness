//! Machine-readable plan step block in `wiki/plan.md`.

use serde::{Deserialize, Serialize};

/// Marker for the structured steps block in `wiki/plan.md`.
pub const PLAN_STEPS_JSON_MARKER: &str = "<!-- plan-steps:json";

/// One step from the `plan-steps:json` comment block.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ParsedPlanStep {
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

/// Serializable mirror of an executing plan step (lib ↔ TUI bridge).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PlanStepData {
    pub description: String,
    #[serde(default)]
    pub verification: Option<String>,
    #[serde(default)]
    pub tier: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub observe_prompt: Option<String>,
    #[serde(default)]
    pub status: String,
}

/// In-memory plan execution state shared between Agent and TUI.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlanExecutionState {
    pub active: bool,
    pub steps: Vec<PlanStepData>,
    pub current_step: usize,
    /// When the deliverable lives in a workspace subdirectory (e.g. `galaga/`), verifications
    /// run from this path unless the command already includes `cd … &&` or `workdir:…|`.
    #[serde(default)]
    pub project_workdir: Option<String>,
    #[serde(default)]
    pub pending_observe_prompt: Option<String>,
    #[serde(default)]
    pub pending_observe_step: Option<usize>,
}

/// Extract the JSON payload from a `<!-- plan-steps:json ... -->` comment block.
pub fn extract_plan_steps_json(content: &str) -> Option<String> {
    let start = content.find(PLAN_STEPS_JSON_MARKER)?;
    let after = &content[start + PLAN_STEPS_JSON_MARKER.len()..];
    let trimmed = after.trim_start();
    let end = trimmed.find("-->")?;
    let json = trimmed[..end].trim();
    if json.is_empty() {
        None
    } else {
        Some(json.to_string())
    }
}

/// Parse structured steps from a JSON comment block.
pub fn parse_plan_steps_json(json: &str) -> Option<Vec<ParsedPlanStep>> {
    let steps: Vec<ParsedPlanStep> = serde_json::from_str(json).ok()?;
    let steps: Vec<ParsedPlanStep> = steps
        .into_iter()
        .filter(|s| !s.description.trim().is_empty())
        .collect();
    if steps.is_empty() {
        None
    } else {
        Some(steps)
    }
}

/// Serialize plan steps into the `wiki/plan.md` JSON comment block.
pub fn format_plan_steps_json_block(steps: &[PlanStepData]) -> String {
    let json_steps: Vec<ParsedPlanStep> = steps
        .iter()
        .map(|st| {
            let (verification, prompt) = if st.tier.as_deref() == Some("observe") {
                (None, st.observe_prompt.clone().or(st.verification.clone()))
            } else {
                (st.verification.clone(), st.observe_prompt.clone())
            };
            ParsedPlanStep {
                description: st.description.clone(),
                tier: st.tier.clone(),
                verification,
                prompt,
                note: st.note.clone(),
            }
        })
        .collect();
    let json = serde_json::to_string_pretty(&json_steps).unwrap_or_else(|_| "[]".to_string());
    format!("{PLAN_STEPS_JSON_MARKER}\n{json}\n-->\n")
}

/// Replace or insert the JSON block inside existing plan markdown.
pub fn upsert_plan_steps_json_block(content: &str, steps: &[PlanStepData]) -> String {
    let block = format_plan_steps_json_block(steps);
    if let Some(start) = content.find(PLAN_STEPS_JSON_MARKER) {
        if let Some(after) = content[start..].find("-->") {
            let end = start + after + 3;
            let mut out = String::with_capacity(content.len() + block.len());
            out.push_str(&content[..start]);
            out.push_str(&block);
            if end < content.len() {
                out.push_str(&content[end..]);
            }
            return out;
        }
    }
    if content.contains("## Steps") {
        format!("{content}\n{block}")
    } else {
        format!("{content}\n\n## Steps\n{block}")
    }
}

const EXECUTION_LOG_MARKER: &str = "## Execution Log";

/// 1-indexed step numbers marked PASS in the wiki execution log.
pub fn parse_execution_log_passed_steps(content: &str) -> Vec<usize> {
    let section = content
        .find(EXECUTION_LOG_MARKER)
        .map(|i| &content[i + EXECUTION_LOG_MARKER.len()..])
        .unwrap_or("");
    let mut steps = Vec::new();
    for line in section.lines() {
        let line = line.trim();
        if !line.starts_with("- Step ") || !line.contains("→ PASS") {
            continue;
        }
        let rest = line.strip_prefix("- Step ").unwrap_or("");
        let num_str: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(n) = num_str.parse::<usize>() {
            if n > 0 {
                steps.push(n);
            }
        }
    }
    steps.sort_unstable();
    steps.dedup();
    steps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execution_log_parses_passed_steps() {
        let md = r#"
## Execution Log
- Step 8 exec `grep texture` → PASS
- Step 4 exec `grep Player` → PASS
- Step 2 exec `grep loop` → PASS
- Step 4 exec duplicate → PASS
"#;
        assert_eq!(parse_execution_log_passed_steps(md), vec![2, 4, 8]);
    }

    #[test]
    fn json_block_roundtrip() {
        let steps = vec![PlanStepData {
            description: "Run tests".to_string(),
            verification: Some("cargo test".to_string()),
            tier: Some("exec".to_string()),
            note: None,
            observe_prompt: None,
            status: "pending".to_string(),
        }];
        let block = format_plan_steps_json_block(&steps);
        let json = extract_plan_steps_json(&block).expect("extract");
        let parsed = parse_plan_steps_json(&json).expect("parse");
        assert_eq!(parsed[0].description, "Run tests");
        assert_eq!(parsed[0].tier.as_deref(), Some("exec"));
    }
}