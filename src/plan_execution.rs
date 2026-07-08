//! Plan step completion and revision (work / plan modes).

pub use crate::plan_md::PlanExecutionState;

use crate::plan_md::{self, ParsedPlanStep, PlanStepData};
use crate::tools::backend::ToolBackend;
use crate::tools::{self, exec_with_exit_code};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const EXEC_VERIFY_TIMEOUT_SECS: u64 = 120;

#[derive(Debug, PartialEq, Eq)]
pub enum CompletePlanStepOutcome {
    Accepted { message: String },
    Failed { message: String },
    NeedsUserObservation { prompt: String, step_index: usize },
    Rejected { message: String },
}

fn tier_name(step: &PlanStepData) -> &str {
    step.tier.as_deref().unwrap_or("exec")
}

/// Names that appear inside project paths but are not the project root directory.
const NESTED_PROJECT_DIR_NAMES: &[&str] = &[
    "src", "include", "assets", "build", "bin", "lib", "dist", "target", "test", "tests",
];

/// Shell/command tokens that are not project directory names when appearing alone.
const STANDALONE_TOKEN_NOISE: &[&str] = &[
    "mkdir", "cmake", "make", "cat", "cd", "true", "false", "grep", "cargo", "clippy", "rustfmt",
    "npm", "pip", "python", "node", "bash", "sh", "exec", "test", "ls", "cp", "mv", "rm", "touch",
];

/// Injection block so the model sees approved steps (stored in agent state, not workspace files).
pub fn format_plan_execution_injection(state: &PlanExecutionState, workspace: &Path) -> String {
    if !state.active || state.steps.is_empty() {
        return String::new();
    }
    let mut block = String::from("### Approved Plan (executing)\n");
    block.push_str(
        "Canonical plan: session wiki `plan.md` (read with wiki=true). \
         Follow the approved steps below — they define which paths to use.\n\n",
    );
    let inferred_workdir = infer_project_workdir_from_steps(&state.steps);
    let workdir = state
        .project_workdir
        .as_deref()
        .or(inferred_workdir.as_deref());
    block.push_str(&format_deliverable_location_section(workspace, workdir));
    block.push('\n');
    for (i, step) in state.steps.iter().enumerate() {
        let n = i + 1;
        let verify = step
            .verification
            .as_deref()
            .or(step.observe_prompt.as_deref())
            .unwrap_or("—");
        let cur = if i == state.current_step {
            " **← CURRENT**"
        } else {
            ""
        };
        block.push_str(&format!(
            "{n}. [{}] {} — verify: {verify} (status: {}){cur}\n",
            tier_name(step),
            step.description,
            step.status
        ));
    }
    if state.current_step < state.steps.len() {
        block.push_str(&format!(
            "\nExecute step {} now, then call `complete_plan_step`.\n",
            state.current_step + 1
        ));
    } else {
        block.push_str(&format!(
            "\nAll {} plan steps are complete. Do not call `complete_plan_step` again.\n",
            state.steps.len()
        ));
    }
    block
}

/// High-signal block clarifying where deliverable files live for the active plan.
pub fn format_deliverable_location_section(workspace: &Path, workdir: Option<&str>) -> String {
    match workdir.filter(|w| !w.is_empty() && *w != ".") {
        Some(wd) => format!(
            "### Deliverable location (critical)\n\
             - Workspace root: `{}`\n\
             - **Project root for this plan:** `{wd}/` (from plan steps)\n\
             - Prefix `write`/`patch`/`read`/`list` paths with `{wd}/` unless the current step's verification shows a different path.\n\
             - Step verifications run with cwd `{wd}/` unless the command includes its own `cd … &&`.\n",
            workspace.display(),
            wd = wd,
        ),
        None => format!(
            "### Deliverable location (critical)\n\
             - **Project root for this plan:** workspace root (`{}`)\n\
             - Use workspace-relative paths as in plan step verifications (e.g. `src/lib.rs`, `Cargo.toml`).\n\
             - `write`/`patch`/`read`/`list` paths are relative to the workspace root.\n\
             - Step verifications run from the workspace root unless the command includes `cd … &&`.\n",
            workspace.display(),
        ),
    }
}

/// Parse a leading `cd <dir> && <rest>` shell prefix (single level, no chaining).
pub fn parse_cd_prefix(command: &str) -> Option<(String, String)> {
    let trimmed = command.trim();
    if !trimmed.starts_with("cd ") {
        return None;
    }
    let rest = trimmed[3..].trim_start();
    let (dir, after_cd) = if let Some(inner) = rest.strip_prefix('"') {
        let end = inner.find('"')?;
        (inner[..end].to_string(), inner[end + 1..].trim_start())
    } else if let Some(inner) = rest.strip_prefix('\'') {
        let end = inner.find('\'')?;
        (inner[..end].to_string(), inner[end + 1..].trim_start())
    } else {
        let (dir_part, after) = rest.split_once("&&")?;
        (dir_part.trim().to_string(), after.trim_start())
    };
    if dir.is_empty() {
        return None;
    }
    Some((dir, after_cd.to_string()))
}

/// Resolve cwd + command for an exec-tier verification.
pub fn resolve_verification_cwd(
    command: &str,
    workspace: &Path,
    project_workdir: Option<&str>,
) -> (PathBuf, String) {
    let cmd = command.trim();
    if let Some(rest) = cmd.strip_prefix("workdir:") {
        if let Some((dir, subcmd)) = rest.split_once('|') {
            let dir = dir.trim();
            if !dir.is_empty() && !subcmd.trim().is_empty() {
                return (workspace.join(dir), subcmd.trim().to_string());
            }
        }
    }
    if let Some((dir, subcmd)) = parse_cd_prefix(cmd) {
        return (workspace.join(dir), subcmd);
    }
    if let Some(wd) = project_workdir {
        if !cmd.starts_with('/') {
            return (workspace.join(wd), cmd.to_string());
        }
    }
    (workspace.to_path_buf(), cmd.to_string())
}

fn subdir_project_score(dir: &Path) -> i32 {
    let mut score = 0;
    if dir.join("src").is_dir() {
        score += 1;
    }
    if dir.join("include").is_dir() {
        score += 1;
    }
    if dir.join("assets").is_dir() {
        score += 1;
    }
    let src = dir.join("src");
    if src.is_dir() {
        if let Ok(entries) = std::fs::read_dir(&src) {
            let mut has_rust = false;
            let mut has_cpp = false;
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "rs") {
                    has_rust = true;
                }
                if path.extension().is_some_and(|e| e == "cpp" || e == "cc" || e == "c") {
                    has_cpp = true;
                }
            }
            if has_rust {
                score -= 5;
            }
            if has_cpp {
                score += 3;
            }
        }
    }
    score
}

fn is_plausible_project_root_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && !name.starts_with('-')
        && !NESTED_PROJECT_DIR_NAMES.contains(&name)
        && !STANDALONE_TOKEN_NOISE.contains(&name)
        && !matches!(name, "target" | "node_modules" | "dist" | "build" | "&&" | "||")
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn record_project_root_candidate(counts: &mut HashMap<String, usize>, name: &str) {
    if is_plausible_project_root_name(name) {
        *counts.entry(name.to_string()).or_default() += 1;
    }
}

fn pick_best_project_root(path_counts: HashMap<String, usize>, standalone_counts: HashMap<String, usize>) -> Option<String> {
    if let Some((name, _)) = path_counts
        .into_iter()
        .max_by_key(|(_, count)| *count)
    {
        return Some(name);
    }
    standalone_counts
        .into_iter()
        .filter(|(_, count)| *count >= 2)
        .max_by_key(|(_, count)| *count)
        .map(|(name, _)| name)
}

fn collect_project_root_candidates(
    text: &str,
    path_counts: &mut HashMap<String, usize>,
    standalone_counts: &mut HashMap<String, usize>,
) {
    for token in text.split(|c: char| {
        c.is_whitespace()
            || matches!(c, '`' | '"' | '\'' | ',' | ';' | '(' | ')' | '>' | '[' | ']')
    }) {
        if token.is_empty() {
            continue;
        }
        if let Some(rest) = token.strip_prefix("./") {
            let dir = rest.split('/').next().unwrap_or(rest);
            record_project_root_candidate(path_counts, dir);
            continue;
        }
        if let Some((dir, _)) = token.split_once('/') {
            record_project_root_candidate(path_counts, dir);
            continue;
        }
        record_project_root_candidate(standalone_counts, token);
    }
}

/// Infer the deliverable subdirectory from plan step text (works before the directory exists).
pub fn infer_project_workdir_from_steps(steps: &[PlanStepData]) -> Option<String> {
    let mut path_counts: HashMap<String, usize> = HashMap::new();
    let mut standalone_counts: HashMap<String, usize> = HashMap::new();
    for step in steps {
        collect_project_root_candidates(&step.description, &mut path_counts, &mut standalone_counts);
        if let Some(v) = step.verification.as_deref() {
            collect_project_root_candidates(v, &mut path_counts, &mut standalone_counts);
        }
        if let Some(n) = step.note.as_deref() {
            collect_project_root_candidates(n, &mut path_counts, &mut standalone_counts);
        }
        if let Some(o) = step.observe_prompt.as_deref() {
            collect_project_root_candidates(o, &mut path_counts, &mut standalone_counts);
        }
    }
    pick_best_project_root(path_counts, standalone_counts)
}

/// Guess the project subdirectory when the deliverable is not at workspace root.
pub fn detect_project_workdir(workspace: &Path, steps: &[PlanStepData]) -> Option<String> {
    let mut best: Option<(String, i32)> = None;
    if let Ok(entries) = std::fs::read_dir(workspace) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.')
                || matches!(name.as_str(), "target" | "node_modules" | "dist" | "build")
            {
                continue;
            }
            let score = subdir_project_score(&path);
            if score >= 3 && best.as_ref().is_none_or(|(_, s)| score > *s) {
                best = Some((name, score));
            }
        }
    }
    if let Some((name, _)) = best {
        return Some(name);
    }
    infer_project_workdir_from_steps(steps)
}

/// Run a verification command and return (passed, output).
pub async fn run_exec_verification(
    command: &str,
    workspace: &Path,
    project_workdir: Option<&str>,
) -> (bool, String) {
    let (cwd, cmd) = resolve_verification_cwd(command, workspace, project_workdir);
    let (code, output) = exec_with_exit_code(&cmd, &cwd, Some(EXEC_VERIFY_TIMEOUT_SECS)).await;
    let passed = code == Some(0);
    (passed, output)
}

/// Run a `check` tier spec (`file_exists:path` or `grep:pattern:path`).
pub fn run_check_verification(
    spec: &str,
    workspace: &Path,
    project_workdir: Option<&str>,
) -> (bool, String) {
    let spec = spec.trim();
    let base = project_workdir
        .map(|wd| workspace.join(wd))
        .unwrap_or_else(|| workspace.to_path_buf());
    if let Some(path) = spec.strip_prefix("file_exists:") {
        let path = path.trim();
        let full = base.join(path);
        let ok = full.is_file();
        return (
            ok,
            if ok {
                format!("file_exists:{path} — found")
            } else {
                format!("file_exists:{path} — not found")
            },
        );
    }
    if let Some(rest) = spec.strip_prefix("grep:") {
        let parts: Vec<&str> = rest.splitn(2, ':').collect();
        if parts.len() == 2 {
            let pattern = parts[0].trim();
            let path = parts[1].trim();
            let out = tools::grep_files(pattern, Some(path), None, 0, false, false, &base);
            let ok = !out.trim().is_empty() && !out.starts_with('❌') && !out.contains("0 matches");
            return (
                ok,
                if ok {
                    format!("grep:{pattern}:{path} — matched")
                } else {
                    format!("grep:{pattern}:{path} — no match\n{out}")
                },
            );
        }
    }
    (
        false,
        format!(
            "Unsupported check spec: {spec}. Use file_exists:path or grep:pattern:path"
        ),
    )
}

fn append_execution_log(session: &mut crate::session::Session, line: &str) {
    let existing = session
        .read_wiki_file_raw("plan.md")
        .unwrap_or_else(|_| String::new());
    let marker = "## Execution Log";
    let entry = format!("\n- {}", line);
    let updated = if let Some(pos) = existing.find(marker) {
        let mut out = existing.clone();
        out.insert_str(pos + marker.len(), &entry);
        out
    } else {
        format!("{existing}\n\n{marker}\n{entry}\n")
    };
    let _ = session.write_wiki_file("plan.md", &updated);
}

fn advance_step(state: &mut PlanExecutionState, step_index: usize, evidence: Option<&str>) {
    if step_index < state.steps.len() {
        state.steps[step_index].status = "done".to_string();
    }
    state.current_step = step_index.saturating_add(1);
    if state.current_step < state.steps.len() {
        state.steps[state.current_step].status = "in_progress".to_string();
    }
    state.pending_observe_prompt = None;
    state.pending_observe_step = None;
    let _ = evidence;
}

fn fail_step(state: &mut PlanExecutionState, step_index: usize) {
    if step_index < state.steps.len() {
        state.steps[step_index].status = "failed".to_string();
    }
}

/// Complete the current (or specified) plan step with tier-appropriate verification.
pub async fn complete_plan_step(
    state: &mut PlanExecutionState,
    backend: &ToolBackend,
    workspace: &Path,
    session: Option<&mut crate::session::Session>,
    args: &Value,
) -> CompletePlanStepOutcome {
    if !state.active || state.steps.is_empty() {
        return CompletePlanStepOutcome::Rejected {
            message: "No approved plan is executing.".to_string(),
        };
    }

    if state.pending_observe_prompt.is_some() {
        return CompletePlanStepOutcome::Rejected {
            message: "An observe step is waiting for user input. Do not call complete_plan_step until the user responds.".to_string(),
        };
    }

    let requested = args
        .get("step")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize);
    let force = args.get("force").and_then(|v| v.as_bool()).unwrap_or(false);
    let evidence = args
        .get("evidence")
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty());

    let step_index = match requested {
        None => state.current_step,
        Some(0) => {
            return CompletePlanStepOutcome::Rejected {
                message: "step must be 1-indexed (>= 1).".to_string(),
            };
        }
        Some(n) => n - 1,
    };

    if step_index >= state.steps.len() {
        return CompletePlanStepOutcome::Rejected {
            message: format!(
                "Step {} does not exist (plan has {} steps).",
                step_index + 1,
                state.steps.len()
            ),
        };
    }

    if state.current_step >= state.steps.len() {
        return CompletePlanStepOutcome::Rejected {
            message: format!(
                "Plan complete — all {} steps are done. Do not call `complete_plan_step` again.",
                state.steps.len()
            ),
        };
    }

    if step_index != state.current_step {
        return CompletePlanStepOutcome::Rejected {
            message: format!(
                "Step {} is not current (working on step {}). Complete steps in order.",
                step_index + 1,
                state.current_step + 1
            ),
        };
    }

    let step = state.steps[step_index].clone();
    let tier = tier_name(&step);

    match tier {
        "observe" => {
            let prompt = step
                .observe_prompt
                .as_deref()
                .or(step.verification.as_deref())
                .unwrap_or("Please confirm this step (yes/no/describe).");
            state.pending_observe_prompt = Some(prompt.to_string());
            state.pending_observe_step = Some(step_index);
            CompletePlanStepOutcome::NeedsUserObservation {
                prompt: prompt.to_string(),
                step_index,
            }
        }
        "attested" => {
            if !force {
                let ev = evidence.unwrap_or("");
                if ev.is_empty() {
                    return CompletePlanStepOutcome::Rejected {
                        message: "attested tier requires non-empty evidence (or force=true).".to_string(),
                    };
                }
                if let Some(s) = session {
                    append_execution_log(
                        s,
                        &format!(
                            "Step {} attested: {}",
                            step_index + 1,
                            ev
                        ),
                    );
                }
                advance_step(state, step_index, evidence);
                return CompletePlanStepOutcome::Accepted {
                    message: format!(
                        "Step {} accepted (attested). Evidence recorded.",
                        step_index + 1
                    ),
                };
            }
            advance_step(state, step_index, evidence);
            CompletePlanStepOutcome::Accepted {
                message: format!("Step {} force-accepted (attested).", step_index + 1),
            }
        }
        "check" => {
            let spec = step.verification.as_deref().unwrap_or("");
            if spec.is_empty() && !force {
                return CompletePlanStepOutcome::Rejected {
                    message: "check tier requires a verification spec (file_exists: or grep:).".to_string(),
                };
            }
            if force {
                advance_step(state, step_index, evidence);
                return CompletePlanStepOutcome::Accepted {
                    message: format!("Step {} force-accepted (check).", step_index + 1),
                };
            }
            let workdir = state.project_workdir.clone();
            let (passed, output) =
                run_check_verification(spec, workspace, workdir.as_deref());
            if passed {
                advance_step(state, step_index, Some(&output));
                CompletePlanStepOutcome::Accepted {
                    message: format!("Step {} passed check: {}", step_index + 1, output),
                }
            } else {
                let hint = verification_failure_hint(spec, workspace, workdir.as_deref());
                fail_step(state, step_index);
                CompletePlanStepOutcome::Failed {
                    message: format!("Step {} check failed: {}\n{hint}", step_index + 1, output),
                }
            }
        }
        _ => {
            // exec (default)
            let command = step.verification.as_deref().unwrap_or("");
            if command.is_empty() && !force {
                return CompletePlanStepOutcome::Rejected {
                    message: "exec tier requires a verification command on the step.".to_string(),
                };
            }
            if force {
                advance_step(state, step_index, evidence);
                return CompletePlanStepOutcome::Accepted {
                    message: format!("Step {} force-accepted (exec).", step_index + 1),
                };
            }
            let (passed, output, used_workdir) =
                run_exec_with_workdir_fallback(command, workspace, state).await;
            if let Some(s) = session {
                append_execution_log(
                    s,
                    &format!(
                        "Step {} exec `{}` → {}",
                        step_index + 1,
                        command,
                        if passed { "PASS" } else { "FAIL" }
                    ),
                );
            }
            let _ = backend;
            if passed {
                advance_step(state, step_index, Some(&output));
                let wd_note = used_workdir
                    .as_ref()
                    .map(|wd| format!(" (from `{wd}/`)"))
                    .unwrap_or_default();
                CompletePlanStepOutcome::Accepted {
                    message: format!(
                        "Step {} passed{wd_note}: {}\n{}",
                        step_index + 1,
                        command,
                        output
                    ),
                }
            } else {
                fail_step(state, step_index);
                let hint = verification_failure_hint(command, workspace, state.project_workdir.as_deref());
                CompletePlanStepOutcome::Failed {
                    message: format!(
                        "Step {} verification failed for `{}`:\n{}\n{hint}",
                        step_index + 1,
                        command,
                        output
                    ),
                }
            }
        }
    }
}

async fn run_exec_with_workdir_fallback(
    command: &str,
    workspace: &Path,
    state: &mut PlanExecutionState,
) -> (bool, String, Option<String>) {
    let workdir = state.project_workdir.as_deref();
    let (passed, output) = run_exec_verification(command, workspace, workdir).await;
    if passed {
        return (true, output, workdir.map(str::to_string));
    }
    if state.project_workdir.is_none() {
        if let Some(detected) = detect_project_workdir(workspace, &state.steps) {
            let (retry_passed, retry_output) =
                run_exec_verification(command, workspace, Some(&detected)).await;
            if retry_passed {
                state.project_workdir = Some(detected.clone());
                return (true, retry_output, Some(detected));
            }
        }
    }
    (false, output, workdir.map(str::to_string))
}

fn verification_failure_hint(command: &str, workspace: &Path, workdir: Option<&str>) -> String {
    let has_cd = parse_cd_prefix(command).is_some() || command.trim_start().starts_with("workdir:");
    let root_label = workspace.display();
    let mut lines = vec![format!(
        "Harness ran verification from `{}`{}.",
        workdir.map(|wd| format!("{root_label}/{wd}"))
            .unwrap_or_else(|| root_label.to_string()),
        if has_cd { " (command includes its own cd/workdir prefix)" } else { "" }
    )];
    if workdir.is_none() && !has_cd {
        if let Some(detected) = detect_project_workdir(workspace, &[]) {
            lines.push(format!(
                "Detected project subdirectory `{detected}/` — use `revise_plan_step` to set verification to `cd {detected} && …` or rely on auto-detection on retry."
            ));
        } else {
            lines.push(
                "If the deliverable lives in a subdirectory, prefix verification with `cd <dir> &&` or call `revise_plan_step` to update the step.".to_string(),
            );
        }
    }
    lines.join("\n")
}

/// Record a user observation for a pending observe step.
pub fn submit_user_observation(
    state: &mut PlanExecutionState,
    user_reply: &str,
    session: Option<&mut crate::session::Session>,
) -> Option<String> {
    let step_index = state.pending_observe_step?;
    let prompt = state.pending_observe_prompt.clone()?;
    if let Some(s) = session {
        append_execution_log(
            s,
            &format!(
                "Step {} observe — Q: {} A: {}",
                step_index + 1,
                prompt,
                user_reply.trim()
            ),
        );
    }
    advance_step(state, step_index, Some(user_reply));
    Some(format!(
        "User observation recorded for step {}. Continuing plan.",
        step_index + 1
    ))
}

/// Revise one step in the wiki plan JSON block.
pub fn revise_plan_step(
    session: &mut crate::session::Session,
    args: &Value,
    exec: Option<&mut PlanExecutionState>,
) -> Result<String, String> {
    let step_num = args
        .get("step")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| "step (1-indexed) is required".to_string())?;
    if step_num < 1 {
        return Err("step must be >= 1".to_string());
    }
    let idx = (step_num - 1) as usize;

    let content = session
        .read_wiki_file_raw("plan.md")
        .map_err(|e| e.to_string())?;
    let json = plan_md::extract_plan_steps_json(&content)
        .ok_or_else(|| "No plan-steps:json block in wiki/plan.md".to_string())?;
    let mut steps = plan_md::parse_plan_steps_json(&json)
        .ok_or_else(|| "Could not parse plan-steps JSON".to_string())?;
    if idx >= steps.len() {
        return Err(format!("step {} out of range (plan has {} steps)", step_num, steps.len()));
    }

    if let Some(t) = args.get("tier").and_then(|v| v.as_str()) {
        steps[idx].tier = Some(t.to_string());
    }
    if let Some(v) = args.get("verification").and_then(|v| v.as_str()) {
        steps[idx].verification = Some(v.to_string());
    }
    if let Some(p) = args.get("prompt").and_then(|v| v.as_str()) {
        steps[idx].prompt = Some(p.to_string());
    }
    if let Some(n) = args.get("note").and_then(|v| v.as_str()) {
        steps[idx].note = Some(n.to_string());
    }
    if let Some(d) = args.get("description").and_then(|v| v.as_str()) {
        steps[idx].description = d.to_string();
    }

    let tier = steps[idx].tier.clone();
    let verification = steps[idx].verification.clone();

    let data: Vec<PlanStepData> = if let Some(state) = exec.as_deref() {
        steps
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let existing = state.steps.get(i);
                PlanStepData {
                    description: s.description.clone(),
                    verification: s.verification.clone(),
                    tier: s.tier.clone(),
                    note: s.note.clone(),
                    observe_prompt: s.prompt.clone(),
                    status: existing
                        .map(|e| e.status.clone())
                        .unwrap_or_else(|| "pending".to_string()),
                }
            })
            .collect()
    } else {
        steps
            .iter()
            .map(|s| PlanStepData {
                description: s.description.clone(),
                verification: s.verification.clone(),
                tier: s.tier.clone(),
                note: s.note.clone(),
                observe_prompt: s.prompt.clone(),
                status: "pending".to_string(),
            })
            .collect()
    };

    if let Some(state) = exec {
        state.steps = data.clone();
        if let Some(wd) = args.get("workdir").and_then(|v| v.as_str()) {
            state.project_workdir = if wd.is_empty() { None } else { Some(wd.to_string()) };
        }
    }

    let updated = plan_md::upsert_plan_steps_json_block(&content, &data);
    session.write_wiki_file("plan.md", &updated);

    Ok(format!(
        "Updated step {} in wiki/plan.md (tier={tier:?}, verification={verification:?})",
        step_num
    ))
}

pub fn plan_step_data_from_parsed(step: ParsedPlanStep) -> PlanStepData {
    let tier = step.tier.clone();
    let (verification, observe_prompt) = if tier.as_deref() == Some("observe") {
        (None, step.prompt.or(step.verification))
    } else {
        (step.verification, step.prompt)
    };
    PlanStepData {
        description: step.description,
        verification,
        tier,
        note: step.note,
        observe_prompt,
        status: "pending".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn check_file_exists() {
        let dir = std::env::temp_dir();
        let (ok, _) =
            run_check_verification("file_exists:definitely_missing_file_abc123", &dir, None);
        assert!(!ok);
    }

    #[test]
    fn parse_cd_prefix_splits_dir_and_command() {
        let (dir, cmd) = parse_cd_prefix("cd galaga && ls src/").expect("parse");
        assert_eq!(dir, "galaga");
        assert_eq!(cmd, "ls src/");
    }

    #[test]
    fn resolve_verification_uses_project_workdir() {
        let ws = Path::new("/tmp/ws");
        let (cwd, cmd) = resolve_verification_cwd("ls src/", ws, Some("galaga"));
        assert_eq!(cwd, Path::new("/tmp/ws/galaga"));
        assert_eq!(cmd, "ls src/");
    }

    #[test]
    fn infer_project_workdir_ignores_test_d_flag_tokens() {
        let steps = vec![PlanStepData {
            description: "Create project directory structure".into(),
            verification: Some("test -d src && test -d include && test -d assets".into()),
            ..Default::default()
        }];
        assert_eq!(infer_project_workdir_from_steps(&steps), None);
    }

    #[test]
    fn infer_project_workdir_from_galaga_plan_steps() {
        let steps = vec![
            PlanStepData {
                description: "Create ./galaga directory structure".into(),
                verification: Some("mkdir -p galaga/src galaga/include galaga/assets".into()),
                ..Default::default()
            },
            PlanStepData {
                description: "Write CMakeLists.txt".into(),
                verification: Some("cat > galaga/CMakeLists.txt".into()),
                ..Default::default()
            },
        ];
        assert_eq!(
            infer_project_workdir_from_steps(&steps).as_deref(),
            Some("galaga")
        );
        let base = std::env::temp_dir().join(format!("raven_plan_infer_{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        assert_eq!(
            detect_project_workdir(&base, &steps).as_deref(),
            Some("galaga")
        );
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn injection_includes_deliverable_location_when_workdir_set() {
        let state = PlanExecutionState {
            active: true,
            project_workdir: Some("galaga".into()),
            steps: vec![PlanStepData {
                description: "Create game".into(),
                status: "in_progress".into(),
                ..Default::default()
            }],
            current_step: 0,
            ..Default::default()
        };
        let block = format_plan_execution_injection(&state, Path::new("/tmp/ws"));
        assert!(block.contains("Deliverable location"));
        assert!(block.contains("galaga/"));
        assert!(block.contains("from plan steps"));
    }

    #[test]
    fn deliverable_location_workspace_root_for_harness_plan() {
        let section = format_deliverable_location_section(Path::new("/tmp/tui"), None);
        assert!(section.contains("workspace root"));
        assert!(section.contains("src/lib.rs"));
        assert!(!section.contains("NOT your deliverable"));

        let steps = vec![PlanStepData {
            description: "Fix plan execution path hints".into(),
            verification: Some("cargo clippy --no-default-features".into()),
            ..Default::default()
        }];
        assert!(infer_project_workdir_from_steps(&steps).is_none());
    }

    #[test]
    fn detect_project_workdir_finds_isolated_subproject() {
        let base = std::env::temp_dir().join(format!("raven_plan_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("galaga/src")).unwrap();
        fs::create_dir_all(base.join("galaga/include")).unwrap();
        fs::create_dir_all(base.join("galaga/assets")).unwrap();
        fs::write(base.join("galaga/src/main.cpp"), "int main(){}").unwrap();
        let detected = detect_project_workdir(&base, &[]);
        assert_eq!(detected.as_deref(), Some("galaga"));
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn injection_lists_steps_and_wiki_hint() {
        let state = PlanExecutionState {
            active: true,
            steps: vec![PlanStepData {
                description: "Create dir".into(),
                verification: Some("mkdir x".into()),
                tier: Some("exec".into()),
                status: "in_progress".into(),
                ..Default::default()
            }],
            current_step: 0,
            ..Default::default()
        };
        let ws = Path::new("/tmp/ws");
        let block = format_plan_execution_injection(&state, ws);
        assert!(block.contains("Create dir"));
        assert!(block.contains("wiki `plan.md`"));
        assert!(block.contains("CURRENT"));
    }

    #[test]
    fn injection_says_complete_when_past_last_step() {
        let state = PlanExecutionState {
            active: true,
            steps: vec![PlanStepData {
                description: "Done".into(),
                status: "done".into(),
                ..Default::default()
            }],
            current_step: 1,
            ..Default::default()
        };
        let block = format_plan_execution_injection(&state, Path::new("/tmp/ws"));
        assert!(block.contains("complete"));
        assert!(!block.contains("CURRENT"));
    }

    #[tokio::test]
    async fn complete_rejects_when_plan_already_done() {
        let mut state = PlanExecutionState {
            active: true,
            steps: vec![PlanStepData {
                description: "a".into(),
                verification: Some("true".into()),
                tier: Some("exec".into()),
                status: "done".into(),
                ..Default::default()
            }],
            current_step: 1,
            ..Default::default()
        };
        let backend = ToolBackend::Mock(crate::tools::backend::MockToolBackend::default());
        let out = complete_plan_step(
            &mut state,
            &backend,
            std::env::temp_dir().as_path(),
            None,
            &serde_json::json!({ "step": 1 }),
        )
        .await;
        match out {
            CompletePlanStepOutcome::Rejected { message } => {
                assert!(message.contains("Plan complete"));
                assert!(!message.contains("step 2"));
            }
            other => panic!("expected rejection, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn complete_rejects_wrong_step_order() {
        let mut state = PlanExecutionState {
            active: true,
            steps: vec![PlanStepData {
                description: "a".into(),
                verification: Some("true".into()),
                tier: Some("exec".into()),
                status: "in_progress".into(),
                ..Default::default()
            }],
            current_step: 0,
            ..Default::default()
        };
        let backend = ToolBackend::Mock(crate::tools::backend::MockToolBackend::default());
        let out = complete_plan_step(
            &mut state,
            &backend,
            std::env::temp_dir().as_path(),
            None,
            &serde_json::json!({ "step": 2 }),
        )
        .await;
        assert!(matches!(out, CompletePlanStepOutcome::Rejected { .. }));
    }
}