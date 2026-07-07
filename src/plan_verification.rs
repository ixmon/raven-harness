//! Plan-step verification quality: detect bad patterns, auto-upgrade, normalize paths.

use crate::plan_execution::infer_project_workdir_from_steps;
use crate::plan_md::PlanStepData;
use crate::plan_protocol::{PlanProposal, PlanProposalStep};

/// Result of harness-side proposal improvement (before user sees the recap).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProposalImprovementReport {
    /// Human-readable descriptions of auto-applied fixes (shown in recap when non-empty).
    pub fixes: Vec<String>,
    /// Issues that could not be fixed automatically — proposal retry may be needed.
    pub errors: Vec<String>,
}

const VERIFICATION_RETRY_NUDGE_HEADER: &str = "\n\nYour proposal had invalid step verifications. Resubmit ONLY the corrected proposal JSON.\n";

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
pub fn improve_proposal(proposal: &mut PlanProposal) -> ProposalImprovementReport {
    let mut report = ProposalImprovementReport::default();
    let workdir = infer_project_workdir_from_steps(&proposal_steps_as_data(&proposal.steps));

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
    fn upgrades_cat_redirect_to_file_exists() {
        let mut p = sample_proposal(vec![PlanProposalStep {
            description: "Create CMakeLists".into(),
            tier: Some("exec".into()),
            verification: Some("cat > galaga/CMakeLists.txt".into()),
            prompt: None,
            note: None,
        }]);
        let r = improve_proposal(&mut p);
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
        let r = improve_proposal(&mut p);
        assert!(r.errors.is_empty());
        assert_eq!(p.steps[0].tier.as_deref(), Some("exec"));
        assert_eq!(
            p.steps[0].verification.as_deref(),
            Some("test -d src && test -d include")
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
        let r = improve_proposal(&mut p);
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