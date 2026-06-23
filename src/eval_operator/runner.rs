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
        if profile == "swebench-live" {
            let status = super::probe::probe_llm(&self.llm_base_url);
            super::probe::preflight_swebench_live(&status)?;
        }
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

        if profile.starts_with("swebench") {
            let status = super::probe::probe_llm(&self.llm_base_url);
            let _ = status.write_json(&log_dir.join("probe.json"));
        }

        for id in ids {
            let t0 = Instant::now();
            let (passed, message) = self.run_one(id, profile, &log_dir)?;
            results.push(ScenarioResult {
                id: id.clone(),
                passed,
                duration_ms: t0.elapsed().as_millis() as u64,
                message,
            });
        }

        let finished_at = Utc::now();
        let passed = results.iter().all(|r| r.passed);

        if profile.starts_with("swebench") {
            let _ = self.aggregate_swebench_scorecard(profile, ids, &log_dir);
        }

        let summary = RunSummary {
            run_id: run_id.clone(),
            profile: profile.to_string(),
            started_at,
            finished_at,
            passed,
            results,
            log_dir: log_dir.clone(),
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

    fn aggregate_swebench_scorecard(
        &self,
        profile: &str,
        ids: &[String],
        log_dir: &Path,
    ) -> Result<()> {
        let script = self.evals_dir().join("swebench/aggregate_scorecard.py");
        if !script.is_file() {
            return Ok(());
        }
        let venv_py = self.evals_dir().join("swebench/.venv/bin/python");
        let python = if venv_py.is_file() {
            venv_py
        } else {
            PathBuf::from("python3")
        };
        let mode = super::swebench::mode_for_profile(profile);
        let out = log_dir.join("scorecard.json");
        let probe_file = log_dir.join("probe.json");
        let mut cmd = Command::new(&python);
        cmd.arg(&script)
            .args(ids)
            .arg("--profile")
            .arg(profile)
            .arg("--mode")
            .arg(&mode)
            .arg("--out")
            .arg(&out);
        if probe_file.is_file() {
            cmd.arg("--probe-file").arg(&probe_file);
        }
        let status = cmd
            .current_dir(&self.manifest_dir)
            .status()
            .with_context(|| format!("aggregate scorecard via {}", script.display()))?;
        if !status.success() {
            eprintln!("warning: scorecard aggregation failed (exit {status})");
        }
        Ok(())
    }

    pub fn run_one(&self, id: &str, profile: &str, log_dir: &Path) -> Result<(bool, String)> {
        let reg = load_registry()?;
        let log_path = log_dir.join(format!("{id}.log"));

        let (status, label) = if id == "replay" {
            self.run_script("run_replay.sh", &log_path)?
        } else if id == "mock_smoke_all" {
            self.run_script("run_mock_smoke.sh", &log_path)?
        } else if id == "unit_tests" {
            self.run_cargo_test(&log_path)?
        } else if super::swebench::is_instance_id(&self.manifest_dir, id) {
            self.run_swebench_instance(id, profile, log_dir, &log_path)?
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
        let prompt_file = self.write_scenario_prompt_file(id, log_path)?;
        let output = Command::new("cargo")
            .args([
                "run", "--release", "--quiet", "--bin", "raven-tui", "--",
                "--prompt-file", prompt_file.to_str().unwrap(),
            ])
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
        profile: &str,
        log_dir: &Path,
        log_path: &PathBuf,
    ) -> Result<(std::process::ExitStatus, String)> {
        let script = self.evals_dir().join("swebench/run_instance.sh");
        let mode = super::swebench::mode_for_profile(profile);
        let mut cmd = Command::new("bash");
        cmd.arg(&script).arg(id);
        if mode == "verify-grade" {
            cmd.arg("--verify-grade");
        } else if mode != "full" {
            anyhow::bail!("unknown SWEBENCH_SMOKE_MODE {mode:?} (use verify-grade or full)");
        }
        cmd.current_dir(&self.manifest_dir)
            .env("LLM_BASE_URL", &self.llm_base_url)
            .env("SWEBENCH_SMOKE_MODE", &mode);
        if let Some(model) = probe_model_id(log_dir) {
            cmd.env("LLM_MODEL", model);
        }

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

        let prompt_file = self.write_scenario_prompt_file(id, log_path)?;

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
                "--prompt-file",
                prompt_file.to_str().unwrap(),
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

    fn write_scenario_prompt_file(&self, id: &str, log_path: &PathBuf) -> Result<PathBuf> {
        let scenario_path = self.evals_dir().join("scenarios").join(format!("{}.json", id));
        let data = std::fs::read_to_string(&scenario_path)
            .with_context(|| format!("read scenario {}", scenario_path.display()))?;
        let v: serde_json::Value = serde_json::from_str(&data)
            .with_context(|| format!("parse scenario {}", scenario_path.display()))?;
        let prompt = v.get("prompt")
            .and_then(|p| p.as_str())
            .ok_or_else(|| anyhow::anyhow!("scenario {} has no prompt field", id))?;

        let prompt_file = log_path.with_file_name(format!("{}_prompt.txt", id));
        std::fs::write(&prompt_file, prompt)
            .with_context(|| format!("write prompt file for {}", id))?;
        Ok(prompt_file)
    }
}

pub fn print_summary(summary: &RunSummary) {
    println!();
    for r in &summary.results {
        let mark = if r.passed { "✓" } else { "✗" };
        println!("{mark} {} ({}ms) — {}", r.id, r.duration_ms, r.message);
    }
    println!();
    if let Some(line) = scorecard_summary_line(&summary.log_dir) {
        println!("{line}");
        println!();
    }
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

fn probe_model_id(log_dir: &Path) -> Option<String> {
    let data = std::fs::read_to_string(log_dir.join("probe.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&data).ok()?;
    v.get("model_id")?.as_str().map(|s| s.to_string())
}

fn scorecard_summary_line(log_dir: &Path) -> Option<String> {
    let path = log_dir.join("scorecard.json");
    let data = std::fs::read_to_string(&path).ok()?;
    let card: serde_json::Value = serde_json::from_str(&data).ok()?;
    let resolved = card["swebench"]["resolved"].as_u64()?;
    let total = card["instance_count"].as_u64()?;
    let rate = card["swebench"]["resolve_rate"].as_f64();
    let med_tools = card["harness"]["median_tool_calls"].as_f64();
    let med_ms = card["harness"]["median_duration_ms"].as_f64();
    let failures = card["failure_modes"].as_object()?;
    let fail_bits: Vec<String> = failures
        .iter()
        .filter(|(k, v)| *k != "resolved" && v.as_u64().unwrap_or(0) > 0)
        .map(|(k, v)| format!("{k}={}", v.as_u64().unwrap_or(0)))
        .collect();
    let rate_pct = rate.map(|r| format!("{:.1}%", r * 100.0)).unwrap_or_else(|| "?".into());
    let model = card["endpoint"]["model_id"].as_str();
    let ctx = card["endpoint"]["context_tokens"].as_u64();
    let mut line = if let (Some(m), Some(c)) = (model, ctx) {
        format!("Scorecard [{m}, {c} ctx]: {resolved}/{total} resolved ({rate_pct})")
    } else {
        format!("Scorecard: {resolved}/{total} resolved ({rate_pct})")
    };
    if let Some(t) = med_tools {
        line.push_str(&format!(", median {t:.0} tool calls"));
    }
    if let Some(ms) = med_ms {
        line.push_str(&format!(", median {:.1}s wall", ms / 1000.0));
    }
    if !fail_bits.is_empty() {
        line.push_str(&format!(" | failures: {}", fail_bits.join(", ")));
    }
    Some(line)
}