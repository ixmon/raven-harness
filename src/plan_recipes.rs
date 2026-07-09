//! Verification recipe catalog for plan drafting.
//!
//! Recipes are table-driven defaults for common step kinds (create file, create
//! dir, acquire assets, build). The LLM still proposes steps; the harness matches
//! recipes to upgrade weak checks and to inject a short card into the proposal
//! system prompt.

use crate::plan_protocol::PlanProposalStep;

/// Coarse classification of a plan step from its description.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepKind {
    CreateFile,
    CreateDir,
    AcquireAssets,
    Build,
    ImplementFeature,
    Other,
}

/// One recommended verification produced by a recipe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecipeCheck {
    pub tier: &'static str,
    pub verification: String,
}

/// Result of matching a step against the catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecipeMatch {
    pub kind: StepKind,
    pub recipe_id: &'static str,
    /// Preferred check when the proposal is missing or too weak.
    pub preferred: Option<RecipeCheck>,
    /// Human-readable reason for an upgrade/block.
    pub note: String,
}

/// Minimum non-empty file size by extension (bytes). Tuned for stubs vs real files.
pub fn min_bytes_floor(path: &str) -> Option<u64> {
    let lower = path.to_lowercase();
    let base = lower.rsplit('/').next().unwrap_or(&lower);
    // Named project files before generic extension rules.
    if base == "cmakelists.txt" || base == "cargo.toml" {
        return Some(40);
    }
    if base == "makefile" || base == "gemfile" {
        return Some(30);
    }
    let ext = lower.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    let floor = match ext {
        "md" | "txt" => 20,
        "h" | "hpp" | "hh" => 40,
        "c" | "cc" | "cpp" | "cxx" => 80,
        "rs" | "py" | "go" | "js" | "ts" | "tsx" | "jsx" => 80,
        "toml" | "cmake" | "json" | "yaml" | "yml" => 40,
        "sh" | "bash" => 40,
        _ => return None,
    };
    Some(floor)
}

/// True when verification greps a build artifact / binary (C++ ABI footgun).
pub fn is_binary_path_grep(verification: &str) -> bool {
    let v = verification.trim();
    let path = if let Some(rest) = v.strip_prefix("grep:") {
        rest.split_once(':').map(|x| x.1).unwrap_or("")
    } else if v.contains("grep") {
        // shell: grep -q 'X' build/galaga
        v.split_whitespace().last().unwrap_or("")
    } else {
        return false;
    };
    let p = path.trim().trim_matches(|c| c == '"' || c == '\'');
    if p.is_empty() {
        return false;
    }
    let lower = p.to_lowercase();
    // Explicit binary-ish locations
    if lower.contains("/build/")
        || lower.starts_with("build/")
        || lower.contains("/target/")
        || lower.starts_with("target/")
        || lower.contains("/dist/")
        || lower.starts_with("dist/")
    {
        return true;
    }
    // No extension and looks like a binary name under build
    if v.contains("build/") && !lower.contains('.') {
        return true;
    }
    // Common binary basenames without source extensions
    let base = lower.rsplit('/').next().unwrap_or(&lower);
    !base.contains('.')
        && (v.contains("build/") || v.contains("grep -q"))
        && !lower.ends_with(".cpp")
        && !lower.ends_with(".h")
        && !lower.ends_with(".rs")
        && !lower.ends_with(".py")
}

/// Classify a step from free-text description.
pub fn classify_step(description: &str) -> StepKind {
    let d = description.to_lowercase();
    if d.contains("download")
        || d.contains("fetch asset")
        || d.contains("kenney")
        || (d.contains("sprite") && (d.contains("get") || d.contains("install") || d.contains("pack")))
        || (d.contains("install") && (d.contains("asset") || d.contains("package") || d.contains("dep")))
    {
        return StepKind::AcquireAssets;
    }
    if d.contains("cmake --build")
        || d.contains("cargo build")
        || d.contains("cargo check")
        || d.contains("compile")
        || d.contains("build project")
        || d.contains("build the")
        || (d.contains("build") && (d.contains("project") || d.contains("binary") || d.contains("target")))
    {
        return StepKind::Build;
    }
    if d.contains("mkdir")
        || d.contains("directory structure")
        || d.contains("create project dir")
        || d.contains("create directories")
        || (d.contains("create") && d.contains("director"))
        || (d.contains("scaffold") && d.contains("dir"))
    {
        return StepKind::CreateDir;
    }
    if d.contains("implement")
        || d.contains("add class")
        || d.contains("add function")
        || d.contains("wire up")
        || d.contains("player ship")
        || d.contains("enemy system")
        || d.contains("collision")
        || d.contains("scoring")
        || d.contains("game loop")
        || d.contains("fixed timestep")
    {
        return StepKind::ImplementFeature;
    }
    if d.contains("create ")
        || d.contains("write ")
        || d.contains("add file")
        || d.contains("cmakelists")
        || d.contains("cargo.toml")
        || d.contains(".cpp")
        || d.contains(".h")
        || d.contains(".rs")
        || d.contains(".py")
    {
        return StepKind::CreateFile;
    }
    StepKind::Other
}

/// Pull a plausible relative path from description or verification text.
pub fn extract_path_hint(description: &str, verification: Option<&str>) -> Option<String> {
    if let Some(v) = verification {
        if let Some(path) = v.strip_prefix("file_exists:") {
            let p = path.trim();
            if !p.is_empty() {
                return Some(p.to_string());
            }
        }
        if let Some(path) = v.strip_prefix("min_bytes:") {
            // min_bytes:path:N
            let parts: Vec<&str> = path.rsplitn(2, ':').collect();
            if parts.len() == 2 {
                let p = parts[1].trim();
                if !p.is_empty() {
                    return Some(p.to_string());
                }
            }
        }
        if let Some(rest) = v.strip_prefix("grep:") {
            if let Some((_, path)) = rest.split_once(':') {
                let p = path.trim();
                if !p.is_empty()
                    && (p.contains('/') || p.contains('.'))
                    && !is_binary_path_grep(v)
                {
                    return Some(p.to_string());
                }
            }
        }
    }

    // Tokens that look like paths in the description
    for token in description.split_whitespace() {
        let t = token.trim_matches(|c: char| {
            matches!(c, ',' | ';' | ':' | '"' | '\'' | '(' | ')' | '`' | '[' | ']')
        });
        if t.contains("..") {
            continue;
        }
        let lower = t.to_lowercase();
        if lower == "cmakelists.txt"
            || lower == "cargo.toml"
            || lower == "makefile"
            || lower.ends_with(".cpp")
            || lower.ends_with(".h")
            || lower.ends_with(".hpp")
            || lower.ends_with(".rs")
            || lower.ends_with(".py")
            || lower.ends_with(".toml")
            || lower.ends_with(".md")
            || (t.contains('/')
                && (lower.contains('.')
                    || lower.ends_with("src")
                    || lower.contains("assets")))
        {
            return Some(t.trim_start_matches("./").to_string());
        }
    }
    None
}

fn create_file_check(path: &str) -> RecipeCheck {
    if let Some(n) = min_bytes_floor(path) {
        RecipeCheck {
            tier: "check",
            verification: format!("min_bytes:{path}:{n}"),
        }
    } else {
        RecipeCheck {
            tier: "check",
            verification: format!("file_exists:{path}"),
        }
    }
}

/// Match a proposal step to a recipe and optional preferred verification.
pub fn match_step(step: &PlanProposalStep) -> RecipeMatch {
    let kind = classify_step(&step.description);
    let verify = step.verification.as_deref();
    let path = extract_path_hint(&step.description, verify);

    match kind {
        StepKind::CreateFile => {
            let preferred = path.as_deref().map(create_file_check);
            RecipeMatch {
                kind,
                recipe_id: "create_file",
                preferred,
                note: "Prove a non-empty file at the planned path (file_exists / min_bytes)"
                    .into(),
            }
        }
        StepKind::CreateDir => {
            let dir = path.unwrap_or_else(|| {
                // Common defaults from descriptions
                if step.description.to_lowercase().contains("sprite") {
                    "assets/sprites".into()
                } else if step.description.to_lowercase().contains("src") {
                    "src".into()
                } else {
                    String::new()
                }
            });
            let preferred = if dir.is_empty() {
                None
            } else {
                Some(RecipeCheck {
                    tier: "exec",
                    verification: format!("test -d {dir}"),
                })
            };
            RecipeMatch {
                kind,
                recipe_id: "create_dir",
                preferred,
                note: "Directory exists under project workdir".into(),
            }
        }
        StepKind::AcquireAssets => {
            let dir = path.unwrap_or_else(|| "assets".into());
            RecipeMatch {
                kind,
                recipe_id: "acquire_assets",
                preferred: Some(RecipeCheck {
                    tier: "exec",
                    verification: format!("test -d {dir} && test -n \"$(ls -A {dir})\""),
                }),
                note: "Acquire steps must not pass on an empty directory".into(),
            }
        }
        StepKind::Build => RecipeMatch {
            kind,
            recipe_id: "build",
            preferred: Some(RecipeCheck {
                tier: "exec",
                verification: "cmake --build build".into(), // generic; model/stack may override
            }),
            note: "Prefer a real build/check command; never grep binaries for symbols".into(),
        },
        StepKind::ImplementFeature => {
            // Prefer non-empty source file if path known; otherwise leave to build step.
            let preferred = path.as_deref().map(create_file_check);
            RecipeMatch {
                kind,
                recipe_id: "implement_feature",
                preferred,
                note: "Implementation needs a real artifact (min_bytes/source) plus a later build step — not binary greps"
                    .into(),
            }
        }
        StepKind::Other => RecipeMatch {
            kind,
            recipe_id: "other",
            preferred: None,
            note: String::new(),
        },
    }
}

/// Whether the current verification is weaker than the recipe preference.
pub fn verification_is_weaker_than_recipe(
    current: Option<&str>,
    preferred: &RecipeCheck,
) -> bool {
    let Some(cur) = current.map(str::trim).filter(|s| !s.is_empty()) else {
        return true;
    };
    if is_binary_path_grep(cur) {
        return true;
    }
    // Creation commands are always weaker
    if cur.starts_with("cat ")
        || cur.starts_with("echo ")
        || cur.starts_with("tee ")
        || cur.starts_with("touch ")
        || cur.starts_with("mkdir ")
    {
        return true;
    }
    // Bare file_exists when recipe wants min_bytes for same path
    if preferred.verification.starts_with("min_bytes:") {
        if let Some(path) = cur.strip_prefix("file_exists:") {
            if preferred.verification.contains(path.trim()) {
                return true;
            }
        }
    }
    // Bare test -d for acquire-style preferred non-empty check
    if preferred.verification.contains("ls -A")
        && (cur.starts_with("test -d") || cur.starts_with("file_exists:"))
        && !cur.contains("ls -A")
    {
        return true;
    }
    false
}

/// Compact recipe card for the proposal system prompt.
pub fn format_recipe_card_for_prompt() -> String {
    r#"## Verification recipes (prefer these; harness will upgrade weak checks)

Create file (CMakeLists.txt, src/foo.cpp, …):
  tier `check`, verification `min_bytes:<path>:<N>` (or `file_exists:<path>`)
  N by extension: headers ~40, source ~80, cmake/toml ~40

Create directory:
  tier `exec`, verification `test -d <path>`

Download / assets:
  tier `exec`, verification `test -d <path> && test -n "$(ls -A <path>)"`
  (empty dir must NOT pass)

Build / compile:
  tier `exec`, verification `cmake --build build` / `cargo check` / stack equivalent
  Do NOT `grep` symbols out of build/ binaries (C++ names are mangled)

Implement feature (player, loop, scoring, …):
  Prefer non-empty source file checks + a later build step
  Avoid sole `grep` on ELF binaries

Forbidden as verification: `cat >`, `echo >`, `tee`, bare `mkdir`, grepping `build/<binary>`."#
        .to_string()
}

/// Apply catalog upgrades to a single step. Returns a fix note when mutated.
pub fn apply_recipe_to_step(step: &mut PlanProposalStep, step_index: usize) -> Option<String> {
    let n = step_index + 1;
    let m = match_step(step);
    let current = step.verification.clone();

    // Always block/rewrite binary greps when we have a better preference or a clear error.
    if let Some(cur) = current.as_deref() {
        if is_binary_path_grep(cur) {
            if let Some(pref) = &m.preferred {
                if !is_binary_path_grep(&pref.verification) {
                    step.tier = Some(pref.tier.to_string());
                    step.verification = Some(pref.verification.clone());
                    return Some(format!(
                        "Step {n}: recipe `{}` replaced binary grep `{cur}` → [{}: {}]",
                        m.recipe_id, pref.tier, pref.verification
                    ));
                }
            }
            // No safe rewrite: strip to a build command for build-like steps
            if m.kind == StepKind::Build || m.kind == StepKind::ImplementFeature {
                // Keep only the build side if shell compound
                if let Some(build_part) = cur
                    .split("&&")
                    .map(str::trim)
                    .find(|p| {
                        p.contains("cmake")
                            || p.contains("cargo")
                            || p.contains("make")
                            || p.contains("ninja")
                    })
                {
                    step.tier = Some("exec".into());
                    step.verification = Some(build_part.to_string());
                    return Some(format!(
                        "Step {n}: removed binary grep from verification; kept `{build_part}`"
                    ));
                }
                step.tier = Some("exec".into());
                step.verification = Some("cmake --build build".into());
                return Some(format!(
                    "Step {n}: recipe `{}` replaced binary symbol grep with build check",
                    m.recipe_id
                ));
            }
        }
    }

    let pref = m.preferred?;
    if !verification_is_weaker_than_recipe(current.as_deref(), &pref) {
        return None;
    }
    let old = current.unwrap_or_else(|| "(none)".into());
    step.tier = Some(pref.tier.to_string());
    step.verification = Some(pref.verification.clone());
    Some(format!(
        "Step {n}: recipe `{}` upgraded `{old}` → [{}: {}]",
        m.recipe_id, pref.tier, pref.verification
    ))
}

/// Apply recipes across all steps; returns fix messages.
pub fn apply_recipes_to_proposal(steps: &mut [PlanProposalStep]) -> Vec<String> {
    let mut fixes = Vec::new();
    for (i, step) in steps.iter_mut().enumerate() {
        let tier = step.tier.as_deref().unwrap_or("exec").to_lowercase();
        if tier == "observe" || tier == "attested" {
            continue;
        }
        if let Some(fix) = apply_recipe_to_step(step, i) {
            fixes.push(fix);
        }
    }
    fixes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_create_file_and_build() {
        assert_eq!(
            classify_step("Create CMakeLists.txt with SDL2"),
            StepKind::CreateFile
        );
        assert_eq!(
            classify_step("Compile project with CMake"),
            StepKind::Build
        );
        assert_eq!(
            classify_step("Implement player ship with movement"),
            StepKind::ImplementFeature
        );
        assert_eq!(
            classify_step("Download Kenney sprite pack"),
            StepKind::AcquireAssets
        );
    }

    #[test]
    fn min_bytes_floors() {
        assert_eq!(min_bytes_floor("src/main.cpp"), Some(80));
        assert_eq!(min_bytes_floor("Player.h"), Some(40));
        assert_eq!(min_bytes_floor("CMakeLists.txt"), Some(40));
        assert_eq!(min_bytes_floor("readme.md"), Some(20));
    }

    #[test]
    fn detects_binary_grep() {
        assert!(is_binary_path_grep(
            "cmake --build build && grep -q 'Player::update' build/galaga"
        ));
        assert!(is_binary_path_grep("grep:Player::update:build/galaga"));
        assert!(!is_binary_path_grep("grep:Player::update:src/player.cpp"));
        assert!(!is_binary_path_grep("file_exists:src/player.cpp"));
    }

    #[test]
    fn upgrades_create_file_to_min_bytes() {
        let mut step = PlanProposalStep {
            description: "Create src/main.cpp with SDL window".into(),
            tier: Some("check".into()),
            verification: Some("file_exists:src/main.cpp".into()),
            prompt: None,
            note: None,
        };
        let fix = apply_recipe_to_step(&mut step, 0).expect("upgrade");
        assert!(fix.contains("min_bytes"), "{fix}");
        assert_eq!(
            step.verification.as_deref(),
            Some("min_bytes:src/main.cpp:80")
        );
    }

    #[test]
    fn replaces_binary_grep_on_implement_step() {
        let mut step = PlanProposalStep {
            description: "Implement player ship with movement and fire".into(),
            tier: Some("exec".into()),
            verification: Some(
                "cmake --build build && grep -q 'Player::update' build/galaga".into(),
            ),
            prompt: None,
            note: None,
        };
        let fix = apply_recipe_to_step(&mut step, 3).expect("rewrite");
        assert!(fix.contains("binary") || fix.contains("removed"), "{fix}");
        let v = step.verification.as_deref().unwrap_or("");
        assert!(!is_binary_path_grep(v), "still binary: {v}");
        assert!(v.contains("cmake") || v.starts_with("min_bytes") || v.starts_with("file_exists"));
    }

    #[test]
    fn recipe_card_mentions_min_bytes_and_forbidden() {
        let card = format_recipe_card_for_prompt();
        assert!(card.contains("min_bytes"));
        assert!(card.contains("build/"));
    }
}
