//! SWE-bench Lite dev smoke integration for `raven-eval` operator state.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
struct InstancesManifest {
    smoke_trio: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct GradeReport {
    resolved: Option<bool>,
    resolved_status: Option<String>,
    #[allow(dead_code)]
    patch_successfully_applied: Option<bool>,
}

pub fn swebench_dir(manifest_dir: &Path) -> PathBuf {
    manifest_dir.join("evals/swebench")
}

pub fn instances_manifest_path(manifest_dir: &Path) -> PathBuf {
    swebench_dir(manifest_dir).join("instances.json")
}

pub fn instance_json_path(manifest_dir: &Path, instance_id: &str) -> PathBuf {
    swebench_dir(manifest_dir)
        .join("instances")
        .join(format!("{instance_id}.json"))
}

pub fn instance_results_dir(manifest_dir: &Path, instance_id: &str) -> PathBuf {
    swebench_dir(manifest_dir)
        .join("results")
        .join(instance_id)
}

/// SWE-bench run mode: `verify-grade` (gold patch) or `full` (live Raven agent).
pub fn smoke_mode() -> String {
    std::env::var("SWEBENCH_SMOKE_MODE").unwrap_or_else(|_| "verify-grade".into())
}

/// Resolve mode from eval profile; `swebench-live` and `easy-bench-live` (which
/// includes a real SWE-bench case) always run the agent ("full"). Other profiles
/// default to "verify-grade" (gold patch smoke test, no agent run).
pub fn mode_for_profile(profile: &str) -> String {
    if profile == "swebench-live" || profile == "easy-bench-live" {
        return "full".into();
    }
    smoke_mode()
}

pub fn smoke_trio_ids(manifest_dir: &Path) -> Result<Vec<String>> {
    let path = instances_manifest_path(manifest_dir);
    let data = std::fs::read_to_string(&path)
        .with_context(|| format!("read {}", path.display()))?;
    let manifest: InstancesManifest = serde_json::from_str(&data)?;
    if manifest.smoke_trio.is_empty() {
        anyhow::bail!("smoke_trio is empty in {}", path.display());
    }
    Ok(manifest.smoke_trio)
}

pub fn is_instance_id(manifest_dir: &Path, id: &str) -> bool {
    instance_json_path(manifest_dir, id).is_file()
}

fn read_grade_report(manifest_dir: &Path, instance_id: &str) -> Result<GradeReport> {
    let path = instance_results_dir(manifest_dir, instance_id).join("report.json");
    let data = std::fs::read_to_string(&path)
        .with_context(|| format!("read {}", path.display()))?;
    serde_json::from_str(&data).context("parse SWE-bench report.json")
}

pub fn instance_passed(manifest_dir: &Path, instance_id: &str) -> Result<bool> {
    let report = read_grade_report(manifest_dir, instance_id)?;
    Ok(report.resolved == Some(true))
}

pub fn format_instance_message(manifest_dir: &Path, instance_id: &str, mode: &str) -> Result<String> {
    let report = read_grade_report(manifest_dir, instance_id)?;
    let results_dir = instance_results_dir(manifest_dir, instance_id);
    if report.resolved == Some(true) {
        Ok(format!("swebench:{mode} resolved"))
    } else {
        let status = report
            .resolved_status
            .unwrap_or_else(|| "unresolved".into());
        Ok(format!(
            "swebench:{mode} {status} (see {})",
            results_dir.display()
        ))
    }
}

pub fn copy_failure_artifacts(
    manifest_dir: &Path,
    instance_id: &str,
    log_dir: &Path,
) -> Result<()> {
    let src = instance_results_dir(manifest_dir, instance_id);
    for name in ["test_output.log", "report.json"] {
        let from = src.join(name);
        if from.is_file() {
            let to = log_dir.join(format!("{instance_id}.{name}"));
            let _ = std::fs::copy(&from, &to);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smoke_trio_loads_three_instances() {
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let ids = smoke_trio_ids(&manifest).expect("load");
        assert_eq!(ids.len(), 3);
        assert!(ids.iter().any(|id| id.contains("marshmallow")));
    }

    #[test]
    fn mode_for_profile_live_vs_smoke() {
        assert_eq!(mode_for_profile("swebench-live"), "full");
        assert_eq!(mode_for_profile("easy-bench-live"), "full");
        assert_eq!(mode_for_profile("swebench-smoke"), "verify-grade");
        assert_eq!(mode_for_profile("single:foo"), "verify-grade");
    }
}