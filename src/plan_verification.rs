//! Plan-step verification quality: detect bad patterns, auto-upgrade, normalize paths.

use crate::plan_execution::infer_project_workdir_from_steps;
use crate::plan_md::PlanStepData;
use crate::plan_protocol::{PlanProposal, PlanProposalStep, PlanQaEntry};
use crate::plan_recipes;

/// Result of harness-side proposal improvement (before user sees the recap).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProposalImprovementReport {
    /// Human-readable descriptions of auto-applied fixes (shown in recap when non-empty).
    pub fixes: Vec<String>,
    /// Issues that could not be fixed automatically — proposal retry may be needed.
    pub errors: Vec<String>,
    /// Non-blocking advisories surfaced to the user (e.g. weak success criteria,
    /// language-convention anti-patterns). Do not force a retry, but warn.
    pub warnings: Vec<String>,
}

const VERIFICATION_RETRY_NUDGE_HEADER: &str = "\n\nYour proposal had invalid step verifications. Resubmit ONLY the corrected proposal JSON.\n";

fn is_plausible_dir_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && !name.starts_with('-')
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
    if let Some(rest) = v.strip_prefix("min_bytes:") {
        if let Some((path, n)) = rest.rsplit_once(':') {
            let path = strip_workdir_prefix(path.trim(), wd);
            return format!("min_bytes:{path}:{}", n.trim());
        }
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
        .filter(|s| !s.is_empty() && !s.starts_with('-'))
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

/// Detect steps whose description indicates an acquire/download and whose
/// verification is a bare `test -d <path>`. Such a check is satisfied by an
/// empty directory (the session log showed an agent creating `assets/sprites/`
/// with only a README to pass `test -d assets/sprites`). We strengthen it to
/// require the directory be non-empty, which is strictly harder to spoof.
fn upgrade_download_verification(
    description: &str,
    verification: &str,
    workdir: Option<&str>,
) -> Option<(String, String)> {
    let desc = description.to_lowercase();
    let is_acquire = desc.contains("download")
        || desc.contains("install ")
        || desc.contains("fetch ")
        || desc.contains("pull ")
        || (desc.contains("get ") && (desc.contains("asset") || desc.contains("sprite") || desc.contains("package")));
    if !is_acquire {
        return None;
    }
    let v = verification.trim();
    // Match `test -d <path>` (single dir, no chained commands).
    let path = v
        .strip_prefix("test -d ")
        .map(|p| p.trim().trim_matches(|c| c == '"' || c == '\''))
        .filter(|p| !p.is_empty() && !p.contains("&&") && !p.contains("||"));
    let path = path?;
    let norm = normalize_verification_paths(path, workdir);
    Some((
        "exec".to_string(),
        format!("test -d {norm} && test -n \"$(ls -A {norm})\""),
    ))
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
                && !verify.starts_with("min_bytes:")
            {
                errors.push(format!(
                    "Step {n}: check tier requires file_exists:, min_bytes:, or grep: (got `{verify}`)"
                ));
            } else if plan_recipes::is_binary_path_grep(verify) {
                errors.push(format!(
                    "Step {n}: verification greps a build/binary path (`{verify}`) — C++ symbols are mangled; use min_bytes:/file_exists: on source or a real build command"
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
    out.push_str("- File scaffold → tier `check`, verification `min_bytes:<path>:<N>` or `file_exists:<path>`.\n");
    out.push_str("- Directory scaffold → tier `exec`, verification `test -d <path>`.\n");
    out.push_str("- Download/assets → non-empty dir (`test -d … && test -n \"$(ls -A …)\"`).\n");
    out.push_str("- Build → tier `exec`, verification `cmake --build …` / `cargo check` / `make`.\n");
    out.push_str("- Never grep symbols out of build/ binaries.\n");
    out.push_str("\nFix these issues:\n");
    for e in errors {
        out.push_str("- ");
        out.push_str(e);
        out.push('\n');
    }
    out
}

/// Build an adversarial-critique nudge appended to the proposal prompt when
/// `improve_proposal` produced *warnings* (weak verifications). Unlike the
/// error nudge, this does not tell the model which specific command to use —
/// it asks the model to reason, for THIS project's stack, whether each
/// verification could be satisfied by a lazy agent without doing the work, and
/// to harden them using its own knowledge of the language/tooling.
///
/// The hardcoded lints that produced the warnings are language-specific seeds;
/// this nudge generalizes the fix to any stack the model understands.
pub fn format_adversarial_critique_nudge(warnings: &[String]) -> String {
    if warnings.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "\n\nYour proposal passed structural validation, but the harness flagged these \
         verification weaknesses:\n",
    );
    for w in warnings {
        out.push_str("- ");
        out.push_str(w);
        out.push('\n');
    }
    out.push_str(
        "\nRe-examine EVERY step's verification with this question: could a lazy agent \
         satisfy it WITHOUT doing the actual work? (e.g. an empty directory passes \
         `test -d`; a symbol exists but does not compile; a pipe masks a failed stage.) \
         Harden each verification so it proves the real outcome for THIS project's \
         language and tooling — use the build/test/typecheck/lint command appropriate \
         to the stack (cargo check, npm test, go build, pytest, tsc --noEmit, etc.) \
         rather than a generic existence check. Resubmit ONLY the corrected proposal \
         JSON with the same steps but stronger verifications.\n",
    );
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

        // Strengthen "download/install" steps verified by a bare `test -d`
        // (satisfied by an empty dir) to require a non-empty directory.
        if let Some(verify) = step.verification.as_deref() {
            if let Some((new_tier, new_verify)) =
                upgrade_download_verification(&step.description, verify, workdir.as_deref())
            {
                report.fixes.push(format!(
                    "Step {n}: strengthened `{verify}` → [{new_tier}: {new_verify}] \
                     (acquire step should not pass on an empty directory)"
                ));
                step.tier = Some(new_tier);
                step.verification = Some(new_verify);
                continue;
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

    // Recipe catalog: upgrade weak create-file / empty-dir / binary-grep checks.
    for fix in plan_recipes::apply_recipes_to_proposal(&mut proposal.steps) {
        report.fixes.push(fix);
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

    // Surface advisories that don't block the proposal but warn the user.
    lint_success_criteria(&proposal.success_criteria, &mut report.warnings);
    for (i, step) in proposal.steps.iter().enumerate() {
        lint_step_verification(step, i, &mut report.warnings);
    }

    report
}

/// Heuristic check of the overall success-criteria string for known footguns:
/// shell pipes that mask exit codes, and commands that never terminate on their own.
fn lint_success_criteria(criteria: &str, warnings: &mut Vec<String>) {
    let trimmed = criteria.trim();
    if trimmed.is_empty() {
        return;
    }
    // A pipe makes only the last stage's exit code count. For a build+run goal
    // this hides build failures (the session log showed `cmake --build build | ./build/galaga`).
    if trimmed.contains('|') && !trimmed.contains("set -o pipefail") {
        warnings.push(
            "Success criteria uses a shell pipe (`|`): only the last stage's exit code is checked. \
             Consider `set -o pipefail` or splitting into separate checks so a failed build \
             is not masked by a later command."
                .to_string(),
        );
    }
    // Commands that are likely non-terminating (games, servers, watchers, REPLs). Running
    // them as a success gate will hang or require an external timeout/kill.
    let lower = trimmed.to_lowercase();
    let non_terminating_markers = [
        "./build/galaga",
        "./galaga",
        "cargo run",
        "npm start",
        "npm run dev",
        "python -m http.server",
        "uvicorn",
        "gunicorn",
        "flask run",
        "rails server",
        "rails s",
        "watch ",
        "tail -f",
        "node server",
        "node app",
    ];
    for marker in non_terminating_markers {
        if lower.contains(marker) && !lower.contains("timeout") {
            warnings.push(format!(
                "Success criteria runs `{marker}` which likely does not exit on its own. \
                 Wrap it with `timeout <N> …` (exit 124 = timeout = OK) or confirm an \
                 exit condition, otherwise the gate will hang."
            ));
            break;
        }
    }
}

/// Per-step advisories: category mismatches and language-convention anti-patterns
/// that `validate_step` does not reject but that the session log showed are harmful.
fn lint_step_verification(step: &PlanProposalStep, index: usize, warnings: &mut Vec<String>) {
    let n = index + 1;
    let desc = step.description.to_lowercase();
    let verify = step.verification.as_deref().unwrap_or("");

    // Category mismatch: "download"/"install"/"fetch" verified by mere existence.
    // The session's step 2 ("Download Kenney sprites") was verified with `test -d`,
    // which the agent satisfied by creating an empty directory.
    let is_acquire = desc.contains("download")
        || desc.contains("install ")
        || desc.contains("fetch ")
        || desc.contains("pull ")
        || desc.contains("get ") && (desc.contains("asset") || desc.contains("sprite") || desc.contains("package"));
    let is_existence = verify.starts_with("test -d")
        || verify.starts_with("test -f")
        || verify.starts_with("file_exists:")
        || verify.starts_with("test -e");
    let already_strengthened = verify.contains("ls -A");
    if is_acquire && is_existence && !already_strengthened {
        warnings.push(format!(
            "Step {n}: \"{acquire}\" verified only by existence (`{verify}`). An empty dir/file \
             satisfies this — prefer a content check (e.g. `test -n \"$(ls -A <dir>/*)\"`) or \
             use the `attested`/`observe` tier so a blocked download is reported honestly.",
            acquire = "acquire/download step"
        ));
    }

    // Language convention: in C/C++ the class definition belongs in the header, not the .cpp.
    // The session showed the agent inserting `class Player {}` into Player.cpp to satisfy
    // `grep:class Player:src/Player.cpp`, which then broke the build (duplicate definition).
    if let Some(rest) = verify.strip_prefix("grep:") {
        if let Some((needle, target)) = rest.split_once(':') {
            let target = target.trim();
            let needle_lower = needle.to_lowercase();
            if (target.ends_with(".cpp") || target.ends_with(".cc") || target.ends_with(".c"))
                && (needle_lower.starts_with("class ")
                    || needle_lower.starts_with("struct ")
                    || needle_lower == "class"
                    || needle_lower == "struct")
            {
                warnings.push(format!(
                    "Step {n}: grep for `{needle}` in `{target}` — for C/C++ the class/struct \
                     definition belongs in the header (`.h`). A lone grep here can be satisfied \
                     by inserting the definition into the `.cpp`, which breaks the build. Prefer \
                     a compile check (`exec` tier) or grep the header instead."
                ));
            }
        }
    }

    // Lone grep on an "implement"/"add" step: a symbol existing ≠ it working.
    // The session's steps 4, 5, 6 all used bare grep for implementation steps.
    let is_implementation = desc.contains("implement")
        || desc.contains("add ")
        || desc.contains("create ")
        && (desc.contains("class") || desc.contains("function") || desc.contains("method"));
    if is_implementation && verify.starts_with("grep:") && !verify.contains("exec") {
        warnings.push(format!(
            "Step {n}: implementation step verified only by `grep` — a symbol existing does not \
             prove it compiles or works. Pair with a build/compile gate (`exec` tier) or move to \
             `attested` with evidence."
        ));
    }

    if plan_recipes::is_binary_path_grep(verify) {
        warnings.push(format!(
            "Step {n}: verification greps a build artifact (`{verify}`). Prefer min_bytes/file_exists \
             on source plus a separate build step."
        ));
    }
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
        // Creation → file_exists, then recipe upgrades to min_bytes for known extensions.
        let v = p.steps[0].verification.as_deref().unwrap_or("");
        assert!(
            v == "file_exists:CMakeLists.txt" || v.starts_with("min_bytes:CMakeLists.txt:"),
            "unexpected verification {v}"
        );
        assert!(!r.fixes.is_empty());
    }

    #[test]
    fn recipe_rewrites_binary_grep_on_implement_step() {
        let mut p = sample_proposal(vec![PlanProposalStep {
            description: "Implement player ship with movement and fire".into(),
            tier: Some("exec".into()),
            verification: Some(
                "cmake --build build && grep -q 'Player::update' build/galaga".into(),
            ),
            prompt: None,
            note: None,
        }]);
        let r = improve_proposal(&mut p, Some("Implement"));
        let v = p.steps[0].verification.as_deref().unwrap_or("");
        assert!(
            !plan_recipes::is_binary_path_grep(v),
            "binary grep should be gone: {v}; fixes={:?}",
            r.fixes
        );
        assert!(
            r.fixes.iter().any(|f| f.contains("recipe") || f.contains("binary") || f.contains("removed")),
            "{:?}",
            r.fixes
        );
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

    #[test]
    fn warns_on_piped_success_criteria() {
        let mut p = sample_proposal(vec![PlanProposalStep {
            description: "Create CMakeLists".into(),
            tier: Some("check".into()),
            verification: Some("file_exists:CMakeLists.txt".into()),
            prompt: None,
            note: None,
        }]);
        p.success_criteria = "cmake --build build | ./build/galaga".into();
        let r = improve_proposal(&mut p, None);
        assert!(r.warnings.iter().any(|w| w.contains("pipe")), "{:?}", r.warnings);
        assert!(r.warnings.iter().any(|w| w.contains("galaga")), "{:?}", r.warnings);
    }

    #[test]
    fn warns_on_download_verified_by_existence() {
        let mut p = sample_proposal(vec![PlanProposalStep {
            description: "Download Kenney.nl sprite pack".into(),
            tier: Some("exec".into()),
            verification: Some("test -d assets/sprites".into()),
            prompt: None,
            note: None,
        }]);
        let r = improve_proposal(&mut p, None);
        // Auto-upgrade strengthens the bare `test -d` to a non-empty check.
        assert!(
            p.steps[0]
                .verification
                .as_deref()
                .unwrap_or("")
                .contains("ls -A"),
            "{:?}",
            p.steps[0].verification
        );
        assert!(!r.fixes.is_empty(), "{:?}", r.fixes);
        // Once strengthened, the "verified only by existence" advisory is suppressed.
        assert!(
            !r.warnings
                .iter()
                .any(|w| w.contains("verified only by existence")),
            "{:?}",
            r.warnings
        );
    }

    #[test]
    fn download_strengthen_respects_workdir() {
        let mut p = sample_proposal(vec![PlanProposalStep {
            description: "Install asset pack".into(),
            tier: Some("exec".into()),
            verification: Some("test -d galaga/assets/sprites".into()),
            prompt: None,
            note: None,
        }]);
        let r = improve_proposal(&mut p, Some("galaga"));
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert_eq!(
            p.steps[0].verification.as_deref(),
            Some("test -d assets/sprites && test -n \"$(ls -A assets/sprites)\"")
        );
    }

    #[test]
    fn non_acquire_test_d_is_not_strengthened() {
        let mut p = sample_proposal(vec![PlanProposalStep {
            description: "Create source directory".into(),
            tier: Some("exec".into()),
            verification: Some("test -d src".into()),
            prompt: None,
            note: None,
        }]);
        let r = improve_proposal(&mut p, None);
        assert!(
            !r.fixes
                .iter()
                .any(|f| f.contains("strengthened")),
            "{:?}",
            r.fixes
        );
        assert_eq!(p.steps[0].verification.as_deref(), Some("test -d src"));
    }

    #[test]
    fn warns_on_grep_class_in_cpp() {
        let mut p = sample_proposal(vec![PlanProposalStep {
            description: "Implement Player class".into(),
            tier: Some("check".into()),
            verification: Some("grep:class Player:src/Player.cpp".into()),
            prompt: None,
            note: None,
        }]);
        let r = improve_proposal(&mut p, None);
        assert!(
            r.warnings.iter().any(|w| w.contains("header") || w.contains(".h")),
            "{:?}",
            r.warnings
        );
    }

    #[test]
    fn no_warning_for_clean_proposal() {
        let mut p = sample_proposal(vec![PlanProposalStep {
            description: "Create CMakeLists".into(),
            tier: Some("check".into()),
            verification: Some("file_exists:CMakeLists.txt".into()),
            prompt: None,
            note: None,
        }]);
        p.success_criteria = "cmake --build build".into();
        let r = improve_proposal(&mut p, None);
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
        assert!(r.errors.is_empty(), "{:?}", r.errors);
    }

    #[test]
    fn adversarial_critique_nudge_empty_when_no_warnings() {
        let nudge = format_adversarial_critique_nudge(&[]);
        assert!(nudge.is_empty());
    }

    #[test]
    fn adversarial_critique_nudge_lists_warnings_and_asks_model_to_harden() {
        let warnings = vec![
            "Step 1: verified only by existence (empty-dir footgun)".into(),
            "Step 2: `grep:class X:*.cpp` — class belongs in header".into(),
        ];
        let nudge = format_adversarial_critique_nudge(&warnings);
        // Each warning is surfaced so the model knows what triggered the critique.
        assert!(nudge.contains("empty-dir footgun"), "{nudge}");
        assert!(nudge.contains("class belongs in header"), "{nudge}");
        // The nudge asks the model to reason about THIS stack, not hardcode a fix.
        assert!(
            nudge.contains("could a lazy agent satisfy it WITHOUT doing the actual work"),
            "{nudge}"
        );
        // It points the model at stack-appropriate tools generically.
        assert!(nudge.contains("cargo check"), "{nudge}");
        assert!(nudge.contains("pytest"), "{nudge}");
        assert!(nudge.contains("tsc --noEmit"), "{nudge}");
    }
}