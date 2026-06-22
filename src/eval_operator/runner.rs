//! Execute eval tiers by delegating to existing `evals/*.sh` scripts.

use anyhow::{Context, Result};
use chrono::Utc;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use super::registry::{load_registry, TestTier};
use super::state::{push_run, results_root, RunSummary, ScenarioResult};

pub struct Runner {
    pub manifest_dir: PathBuf,
    pub llm_base_url: String,
}

impl Default for Runner {
    fn default() -> Self {
        Self {
            manifest_dir: PathBuf::from(env!("CARGO_MANIFEST_DIR")),
            llm_base_url: super::probe::default_base_url(),
        }
    }
}

impl Runner {
    pub fn new() -> Self {
        Self::default()
    }

    fn evals_dir(&self) -> PathBuf {
        self.manifest_dir.join("evals")
    }

    pub fn run_profile(&self, profile: &str, state: &mut super::state::OperatorState) -> Result<RunSummary> {
        let ids = super::registry::profile_ids(profile)?;
        self.run_ids(profile, &ids, state)
    }

    pub fn run_profile_filtered(
        &self,
        profile: &str,
        llm: &super::probe::LlmStatus,
        state: &mut super::state::OperatorState,
    ) -> Result<RunSummary> {
        let mut ids = super::registry::profile_ids(profile)?;
        if !llm.reachable {
            let reg = super::registry::load_registry()?;
            ids.retain(|id| {
                super::registry::find_entry(&reg, id)
                    .map(|e| !e.needs_llm)
                    .unwrap_or(true)
            });
        }
        self.run_ids(profile, &ids, state)
    }

    pub fn run_ids(
        &self,
        profile: &str,
        ids: &[String],
        state: &mut super::state::OperatorState,
    ) -> Result<RunSummary> {
        let run_id = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
        let log_dir = results_root().join(&run_id);
        std::fs::create_dir_all(&log_dir)?;

        let started_at = Utc::now();
        let mut results = vec![];

        for id in ids {
            let t0 = Instant::now();
            let (passed, message) = self.run_one(id, &log_dir)?;
            results.push(ScenarioResult {
                id: id.clone(),
                passed,
                duration_ms: t0.elapsed().as_millis() as u64,
                message,
            });
        }

        let finished_at = Utc::now();
        let passed = results.iter().all(|r| r.passed);
        let summary = RunSummary {
            run_id: run_id.clone(),
            profile: profile.to_string(),
            started_at,
            finished_at,
            passed,
            results,
            log_dir,
        };

        let summary_path = summary.log_dir.join("summary.json");
        std::fs::write(
            &summary_path,
            serde_json::to_string_pretty(&summary)?,
        )?;

        push_run(state, summary.clone());
        super::state::save_state(state)?;
        Ok(summary)
    }

    pub fn run_one(&self, id: &str, log_dir: &Path) -> Result<(bool, String)> {
        let reg = load_registry()?;
        let log_path = log_dir.join(format!("{id}.log"));

        let (status, label) = if id == "replay" {
            self.run_script("run_replay.sh", &log_path)?
        } else if id == "mock_smoke_all" {
            self.run_script("run_mock_smoke.sh", &log_path)?
        } else if id == "unit_tests" {
            self.run_cargo_test(&log_path)?
        } else if super::swebench::is_instance_id(&self.manifest_dir, id) {
            self.run_swebench_instance(id, log_dir, &log_path)?
        } else if let Some(entry) = super::registry::find_entry(&reg, id) {
            match entry.tier {
                TestTier::Replay => {
                    return Ok((false, format!("scenario {id} is replay-only; use replay tier")));
                }
                TestTier::MockSmoke => self.run_mock_scenario(id, &log_path)?,
                TestTier::LiveSmoke => self.run_live_scenario(id, &log_path)?,
            }
        } else {
            return Ok((false, format!("unknown test id {id}")));
        };

        let msg = format!("{label} (exit {})", status.code().unwrap_or(-1));
        Ok((status.success(), msg))
    }

    fn run_script(&self, script: &str, log_path: &PathBuf) -> Result<(std::process::ExitStatus, String)> {
        let path = self.evals_dir().join(script);
        let output = Command::new("bash")
            .arg(&path)
            .current_dir(&self.manifest_dir)
            .env("LLM_BASE_URL", &self.llm_base_url)
            .output()
            .with_context(|| format!("run {}", path.display()))?;
        std::fs::write(log_path, &output.stdout)?;
        let _ = std::fs::write(
            log_path.with_extension("err.log"),
            &output.stderr,
        );
        Ok((output.status, script.to_string()))
    }

    fn run_cargo_test(&self, log_path: &PathBuf) -> Result<(std::process::ExitStatus, String)> {
        let output = Command::new("cargo")
            .args(["test", "--no-default-features", "--quiet"])
            .current_dir(&self.manifest_dir)
            .output()
            .context("cargo test")?;
        std::fs::write(log_path, &output.stdout)?;
        let _ = std::fs::write(
            log_path.with_extension("err.log"),
            &output.stderr,
        );
        Ok((output.status, "unit_tests".into()))
    }

    fn run_mock_scenario(&self, id: &str, log_path: &PathBuf) -> Result<(std::process::ExitStatus, String)> {
        let output = Command::new("cargo")
            .args(["run", "--release", "--quiet", "--bin", "raven-tui"])
            .current_dir(&self.manifest_dir)
            .env("RAVEN_EVAL", "1")
            .env("RAVEN_EVAL_MOCK_LLM", "1")
            .env("RAVEN_EVAL_SCENARIO", id)
            .env("RAVEN_EVAL_ASSERT_STRICT", "1")
            .output()
            .context("mock scenario")?;
        std::fs::write(log_path, &output.stdout)?;
        let _ = std::fs::write(
            log_path.with_extension("err.log"),
            &output.stderr,
        );
        Ok((output.status, format!("mock:{id}")))
    }

    fn run_swebench_instance(
        &self,
        id: &str,
        log_dir: &Path,
        log_path: &PathBuf,
    ) -> Result<(std::process::ExitStatus, String)> {
        let script = self.evals_dir().join("swebench/run_instance.sh");
        let mode = super::swebench::smoke_mode();
        let mut cmd = Command::new("bash");
        cmd.arg(&script).arg(id);
        if mode == "verify-grade" {
            cmd.arg("--verify-grade");
        } else if mode != "full" {
            anyhow::bail!("unknown SWEBENCH_SMOKE_MODE {mode:?} (use verify-grade or full)");
        }
        cmd.current_dir(&self.manifest_dir);

        let output = cmd
            .output()
            .with_context(|| format!("run {}", script.display()))?;
        std::fs::write(log_path, &output.stdout)?;
        let err_path = log_path.with_extension("err.log");
        std::fs::write(&err_path, &output.stderr)?;

        if !output.status.success() {
            let _ = super::swebench::copy_failure_artifacts(&self.manifest_dir, id, log_dir);
        }

        let message = super::swebench::format_instance_message(&self.manifest_dir, id, &mode)
            .unwrap_or_else(|e| format!("swebench:{mode} (report unreadable: {e})"));
        Ok((output.status, message))
    }

    fn run_live_scenario(&self, id: &str, log_path: &PathBuf) -> Result<(std::process::ExitStatus, String)> {
        let workspace = std::env::temp_dir().join(format!(
            "raven-eval-live-{}-{id}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&workspace);

        let output = Command::new("cargo")
            .args([
                "run",
                "--release",
                "--quiet",
                "--bin",
                "raven-tui",
                "--",
                "--base-url",
                &self.llm_base_url,
                "--temperature",
                "0",
            ])
            .current_dir(&self.manifest_dir)
            .env("RAVEN_EVAL", "1")
            .env("RAVEN_EVAL_SCENARIO", id)
            .env("RAVEN_EVAL_WORKSPACE", &workspace)
            .output()
            .context("live scenario")?;
        let _ = std::fs::remove_dir_all(&workspace);
        std::fs::write(log_path, &output.stdout)?;
        let _ = std::fs::write(
            log_path.with_extension("err.log"),
            &output.stderr,
        );
        Ok((output.status, format!("live:{id}")))
    }
}

pub fn print_summary(summary: &RunSummary) {
    println!();
    for r in &summary.results {
        let mark = if r.passed { "✓" } else { "✗" };
        println!("{mark} {} ({}ms) — {}", r.id, r.duration_ms, r.message);
    }
    println!();
    if summary.passed {
        println!(
            "PASS — {} ({} items)",
            summary.profile,
            summary.results.len()
        );
    } else {
        println!(
            "FAIL — {} (see {})",
            summary.profile,
            summary.log_dir.display()
        );
    }
}