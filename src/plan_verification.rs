//! Plan-step verification quality: detect bad patterns, auto-upgrade, normalize paths.

use crate::plan_execution::infer_project_workdir_from_steps;
use crate::plan_md::PlanStepData;
use crate::plan_protocol::{PlanProposal, PlanProposalStep, PlanQaEntry};

/// Result of harness-side proposal improvement (before user sees the recap).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProposalImprovementReport {
    /// Human-readable descriptions of auto-applied fixes (shown in recap when non-empty).
    pub fixes: Vec<String>,
    /// Issues that could not be fixed automatically — proposal retry may be needed.
    pub errors: Vec<String>,
}

const VERIFICATION_RETRY_NUDGE_HEADER: &str = "\n\nYour proposal had invalid step verifications. Resubmit ONLY the corrected proposal JSON.\n";

fn is_plausible_dir_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        && !matches!(
            name,
            "src" | "include" | "build" | "the" | "here" | "a" | "an" | "this" | "that"
        )
}

/// Extract `./dirname` from free text (e.g. user said "everything in ./galaga/").
pub fn extract_project_workdir_from_text(text: &str) -> Option<String> {
    let mut search = 0usize;
    while let Some(rel) = text[search..].find("./") {
        let start = search + rel + 2;
        let rest = &text[start..];
        let name: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
            .collect();
        if is_plausible_dir_name(&name) {
            return Some(name);
        }
        search = start;
    }

    let lower = text.to_lowercase();
    for marker in [
        "subdirectory here ",
        "subdirectory ",
        "project directory ",
        "project root ",
        "directory ",
    ] {
        if let Some(idx) = lower.find(marker) {
            let rest = &text[idx + marker.len()..];
            if let Some(wd) = extract_project_workdir_from_text(rest) {
                return Some(wd);
            }
            let name: String = rest
                .trim()
                .trim_matches(|c: char| matches!(c, '.' | '/' | '`' | '"'))
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
                .collect();
            if is_plausible_dir_name(&name) {
                return Some(name);
            }
        }
    }

    None
}

/// Prefer explicit user directory choices from clarify Q&A, then the initial request.
pub fn resolve_project_workdir_from_context(
    initial_request: &str,
    history: &[PlanQaEntry],
) -> Option<String> {
    for entry in history.iter().rev() {
        if let Some(wd) = extract_project_workdir_from_text(&entry.user_input) {
            return Some(wd);
        }
        if let Some(wd) = extract_project_workdir_from_text(&entry.resolution) {
            return Some(wd);
        }
    }
    extract_project_workdir_from_text(initial_request)
}

/// Resolve deliverable subdirectory: user context first, then step-path inference.
pub fn resolve_project_workdir(
    initial_request: &str,
    history: &[PlanQaEntry],
    steps: &[PlanProposalStep],
) -> Option<String> {
    resolve_project_workdir_from_context(initial_request, history).or_else(|| {
        infer_project_workdir_from_steps(&proposal_steps_as_data(steps))
    })
}

/// Prompt section injected when the user named a project subdirectory.
pub fn format_project_workdir_prompt_section(workdir: &str) -> String {
    format!(
        "\n\n**User-specified project directory:** `{workdir}/`\n\
         - ALL source files, build files, and assets MUST live under `{workdir}/`.\n\
         - Step verifications use paths relative to `{workdir}/` (e.g. `src/main.cpp`, not `{workdir}/src/main.cpp`).\n\
         - Put this in `constraints` (e.g. \"All deliverables under `{workdir}/`\").\n\
         - First step should create `{workdir}/` if it does not exist yet.\n"
    )
}

fn ensure_constraints_mention_workdir(proposal: &mut PlanProposal, workdir: &str) {
    let note = format!("All deliverables live under `{workdir}/` (project root for this plan).");
    match proposal.constraints.as_mut() {
        Some(c) if c.contains(workdir) => {}
        Some(c) => {
            if !c.trim().is_empty() {
                c.push('\n');
            }
            c.push_str(&note);
        }
        None => {
            proposal.constraints = Some(note);
        }
    }
}

/// True when verification replays file/directory creation instead of proving an outcome.
pub fn is_creation_verification(verification: &str) -> bool {
    let v = verification.trim().to_lowercase();
    if v.is_empty() {
        return false;
    }
    v.starts_with("cat ")
        || v.contains("> ")
        || v.contains(">>")
        || v.starts_with("tee ")
        || v.starts_with("touch ")
        || v.starts_with("mkdir ")
        || v.contains("<<")
}

/// True when verification is weak and should be flagged in the user recap.
pub fn is_weak_verification(verification: &str, tier: Option<&str>) -> bool {
    let tier = tier.unwrap_or("exec");
    if tier == "observe" || tier == "attested" {
        return false;
    }
    let v = verification.trim();
    if v.is_empty() {
        return true;
    }
    if tier == "check" {
        return !(v.starts_with("file_exists:") || v.starts_with("grep:"));
    }
    is_creation_verification(v) || v == "true" || v == "false"
}

fn proposal_steps_as_data(steps: &[PlanProposalStep]) -> Vec<PlanStepData> {
    steps
        .iter()
        .map(|st| PlanStepData {
            description: st.description.clone(),
            verification: st.verification.clone(),
            tier: st.tier.clone(),
            note: st.note.clone(),
            observe_prompt: st.prompt.clone(),
            status: String::new(),
        })
        .collect()
}

fn strip_workdir_prefix(path: &str, workdir: &str) -> String {
    let trimmed = path.trim().trim_matches(|c| c == '"' || c == '\'');
    for prefix in [
        format!("{workdir}/"),
        format!("./{workdir}/"),
        format!("{workdir}\\"),
    ] {
        if let Some(rest) = trimmed.strip_prefix(&prefix) {
            return rest.to_string();
        }
    }
    if trimmed == workdir || trimmed == format!("./{workdir}") {
        return ".".to_string();
    }
    trimmed.to_string()
}

fn normalize_verification_paths(verification: &str, workdir: Option<&str>) -> String {
    let Some(wd) = workdir.filter(|w| !w.is_empty() && *w != ".") else {
        return verification.to_string();
    };
    let v = verification.trim();
    if let Some(rest) = v.strip_prefix("file_exists:") {
        let path = strip_workdir_prefix(rest, wd);
        return format!("file_exists:{path}");
    }
    if let Some(rest) = v.strip_prefix("grep:") {
        let parts: Vec<&str> = rest.splitn(2, ':').collect();
        if parts.len() == 2 {
            let pattern = parts[0].trim();
            let path = strip_workdir_prefix(parts[1], wd);
            return format!("grep:{pattern}:{path}");
        }
    }
    if let Some((dir, sub)) = crate::plan_execution::parse_cd_prefix(v) {
        if dir == wd {
            return sub;
        }
        return verification.to_string();
    }
    // Rewrite path tokens inside shell commands (galaga/src/foo -> src/foo).
    let mut out = v.to_string();
    for token in v.split_whitespace() {
        let clean = token.trim_matches(|c: char| matches!(c, '"' | '\'' | '(' | ')' | ';'));
        if clean.contains('/') {
            let normalized = strip_workdir_prefix(clean, wd);
            if normalized != clean {
                out = out.replace(clean, &normalized);
            }
        }
    }
    out
}

fn extract_write_target(verification: &str) -> Option<String> {
    let v = verification.trim();
    if let Some(idx) = v.find('>') {
        let path = v[idx + 1..].trim();
        let path = path.split_whitespace().next()?.trim_matches(|c| c == '"' || c == '\'');
        if !path.is_empty() && !path.starts_with('&') {
            return Some(path.to_string());
        }
    }
    if let Some(rest) = v.strip_prefix("tee ") {
        let path = rest.split_whitespace().next()?.trim_matches(|c| c == '"' || c == '\'');
        if !path.is_empty() {
            return Some(path.to_string());
        }
    }
    if let Some(rest) = v.strip_prefix("touch ") {
        let path = rest.split_whitespace().next()?.trim_matches(|c| c == '"' || c == '\'');
        if !path.is_empty() {
            return Some(path.to_string());
        }
    }
    None
}

fn extract_mkdir_dirs(verification: &str) -> Vec<String> {
    let v = verification.trim();
    let rest = v
        .strip_prefix("mkdir -p ")
        .or_else(|| v.strip_prefix("mkdir "))
        .unwrap_or(v);
    rest.split_whitespace()
        .map(|s| s.trim_matches(|c| c == '"' || c == '\''))
        .filter(|s| !s.is_empty() && *s != "-p")
        .map(|s| s.to_string())
        .collect()
}

fn upgrade_creation_verification(
    verification: &str,
    workdir: Option<&str>,
) -> Option<(String, String)> {
    if let Some(path) = extract_write_target(verification) {
        let path = normalize_verification_paths(&path, workdir);
        return Some((
            "check".to_string(),
            format!("file_exists:{path}"),
        ));
    }
    if verification.trim().starts_with("mkdir ") {
        let dirs: Vec<String> = extract_mkdir_dirs(verification)
            .into_iter()
            .map(|d| normalize_verification_paths(&d, workdir))
            .collect();
        if dirs.is_empty() {
            return None;
        }
        let tests: Vec<String> = dirs.iter().map(|d| format!("test -d {d}")).collect();
        return Some(("exec".to_string(), tests.join(" && ")));
    }
    None
}

fn validate_step(step: &PlanProposalStep, index: usize, errors: &mut Vec<String>) {
    let n = index + 1;
    if step.description.trim().is_empty() {
        errors.push(format!("Step {n}: missing description"));
        return;
    }
    let tier = step.tier.as_deref().unwrap_or("exec").to_lowercase();
    match tier.as_str() {
        "observe" => {
            let prompt = step
                .prompt
                .as_deref()
                .or(step.verification.as_deref())
                .unwrap_or("");
            if prompt.trim().is_empty() {
                errors.push(format!(
                    "Step {n}: observe tier requires a non-empty prompt"
                ));
            }
        }
        "attested" => {
            if step.note.as_deref().unwrap_or("").trim().is_empty() {
                errors.push(format!(
                    "Step {n}: attested tier requires a note explaining the fallback"
                ));
            }
        }
        _ => {
            let verify = step.verification.as_deref().unwrap_or("");
            if verify.trim().is_empty() {
                errors.push(format!("Step {n}: requires a verification spec"));
            } else if is_creation_verification(verify) {
                errors.push(format!(
                    "Step {n}: verification must prove outcome, not replay creation (`{verify}`)"
                ));
            } else if tier == "check"
                && !verify.starts_with("file_exists:")
                && !verify.starts_with("grep:")
            {
                errors.push(format!(
                    "Step {n}: check tier requires file_exists: or grep: (got `{verify}`)"
                ));
            }
        }
    }
}

/// Build a nudge appended to the proposal user prompt when validation fails.
pub fn format_validation_retry_nudge(errors: &[String]) -> String {
    if errors.is_empty() {
        return String::new();
    }
    let mut out = VERIFICATION_RETRY_NUDGE_HEADER.to_string();
    out.push_str("Rules:\n");
    out.push_str("- Verification proves the step worked — never replay creation (`cat >`, `echo >`, `tee`, bare `mkdir`).\n");
    out.push_str("- File scaffold → tier `check`, verification `file_exists:<path>`.\n");
    out.push_str("- Directory scaffold → tier `exec`, verification `test -d <path>`.\n");
    out.push_str("- Code/content → tier `check`, verification `grep:<symbol>:<path>`.\n");
    out.push_str("- Build → tier `exec`, verification `cmake --build …` / `cargo check` / `make`.\n");
    out.push_str("\nFix these issues:\n");
    for e in errors {
        out.push_str("- ");
        out.push_str(e);
        out.push('\n');
    }
    out
}

/// Auto-upgrade weak verifications and normalize paths. Mutates `proposal` in place.
pub fn improve_proposal(
    proposal: &mut PlanProposal,
    context_workdir: Option<&str>,
) -> ProposalImprovementReport {
    let mut report = ProposalImprovementReport::default();
    let workdir = context_workdir
        .map(str::to_string)
        .or_else(|| infer_project_workdir_from_steps(&proposal_steps_as_data(&proposal.steps)));

    if let Some(wd) = workdir.as_deref() {
        ensure_constraints_mention_workdir(proposal, wd);
    }

    for (i, step) in proposal.steps.iter_mut().enumerate() {
        let n = i + 1;
        let tier = step.tier.as_deref().unwrap_or("exec").to_lowercase();

        if tier == "observe" || tier == "attested" {
            continue;
        }

        if let Some(verify) = step.verification.as_deref() {
            if is_creation_verification(verify) {
                if let Some((new_tier, new_verify)) =
                    upgrade_creation_verification(verify, workdir.as_deref())
                {
                    report.fixes.push(format!(
                        "Step {n}: upgraded `{verify}` → [{new_tier}: {new_verify}]"
                    ));
                    step.tier = Some(new_tier);
                    step.verification = Some(new_verify);
                    continue;
                }
            }
        }

        if let Some(verify) = step.verification.take() {
            let normalized = normalize_verification_paths(&verify, workdir.as_deref());
            if normalized != verify {
                report.fixes.push(format!(
                    "Step {n}: normalized verification paths for project root"
                ));
            }
            step.verification = Some(normalized);
        }
    }

    for (i, v) in proposal.verification.iter_mut().enumerate() {
        let normalized = normalize_verification_paths(v, workdir.as_deref());
        if normalized != *v {
            report.fixes.push(format!(
                "Goal verification {}: normalized paths for project root",
                i + 1
            ));
            *v = normalized;
        }
    }

    for (i, step) in proposal.steps.iter().enumerate() {
        validate_step(step, i, &mut report.errors);
    }

    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan_protocol::PlanProposal;

    fn sample_proposal(steps: Vec<PlanProposalStep>) -> PlanProposal {
        PlanProposal {
            goal: "Build galaga".into(),
            success_criteria: "Compiles".into(),
            verification: vec![],
            rollback: None,
            constraints: None,
            steps,
        }
    }

    #[test]
    fn extracts_galaga_from_dot_slash() {
        assert_eq!(
            extract_project_workdir_from_text("how about a subdirectory here ./galaga"),
            Some("galaga".into())
        );
        assert_eq!(
            extract_project_workdir_from_text("Build game in ./galaga/ directory"),
            Some("galaga".into())
        );
        assert!(extract_project_workdir_from_text("similar to the galaga video game").is_none());
    }

    #[test]
    fn context_workdir_wins_over_step_inference() {
        let history = vec![crate::plan_protocol::PlanQaEntry {
            question_id: "loc".into(),
            question_prompt: "Where?".into(),
            user_input: "use ./galaga".into(),
            resolution: "Free text: use ./galaga".into(),
        }];
        let wd = resolve_project_workdir_from_context("build a galaga clone", &history);
        assert_eq!(wd.as_deref(), Some("galaga"));
    }

    #[test]
    fn upgrades_cat_redirect_to_file_exists() {
        let mut p = sample_proposal(vec![PlanProposalStep {
            description: "Create CMakeLists".into(),
            tier: Some("exec".into()),
            verification: Some("cat > galaga/CMakeLists.txt".into()),
            prompt: None,
            note: None,
        }]);
        let r = improve_proposal(&mut p, Some("galaga"));
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert_eq!(p.steps[0].tier.as_deref(), Some("check"));
        assert_eq!(
            p.steps[0].verification.as_deref(),
            Some("file_exists:CMakeLists.txt")
        );
        assert!(!r.fixes.is_empty());
    }

    #[test]
    fn upgrades_mkdir_to_test_d() {
        let mut p = sample_proposal(vec![PlanProposalStep {
            description: "Create dirs".into(),
            tier: Some("exec".into()),
            verification: Some("mkdir -p galaga/src galaga/include".into()),
            prompt: None,
            note: None,
        }]);
        let r = improve_proposal(&mut p, Some("galaga"));
        assert!(r.errors.is_empty());
        assert_eq!(p.steps[0].tier.as_deref(), Some("exec"));
        assert_eq!(
            p.steps[0].verification.as_deref(),
            Some("test -d src && test -d include")
        );
    }

    #[test]
    fn ensure_constraints_mentions_workdir() {
        let mut p = sample_proposal(vec![]);
        improve_proposal(&mut p, Some("galaga"));
        assert!(
            p.constraints
                .as_deref()
                .is_some_and(|c| c.contains("galaga/"))
        );
    }

    #[test]
    fn detects_empty_verification_error() {
        let mut p = sample_proposal(vec![PlanProposalStep {
            description: "Do something".into(),
            tier: Some("exec".into()),
            verification: None,
            prompt: None,
            note: None,
        }]);
        let r = improve_proposal(&mut p, None);
        assert_eq!(r.errors.len(), 1);
        assert!(r.errors[0].contains("requires a verification"));
    }

    #[test]
    fn is_weak_flags_creation_commands() {
        assert!(is_weak_verification("cat > foo.txt", Some("exec")));
        assert!(!is_weak_verification("file_exists:foo.txt", Some("check")));
        assert!(!is_weak_verification("cargo check", Some("exec")));
    }

    #[test]
    fn format_retry_nudge_lists_errors() {
        let nudge = format_validation_retry_nudge(&["Step 2: bad verify".into()]);
        assert!(nudge.contains("file_exists"));
        assert!(nudge.contains("Step 2"));
    }
}