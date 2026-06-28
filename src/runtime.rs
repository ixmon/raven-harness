//! Centralized runtime configuration.
//!
//! **Rule: no module below `main` should ever call `std::env::var` for behavioral
//! decisions.** All environment variables are read once in `main.rs`, packed into
//! these structs, and threaded through `Config`.
//!
//! There are two structs, reflecting two distinct configuration surfaces:
//!
//! 1. [`RuntimeFlags`] — behavioral switches that control *how the agent acts*.
//!    These determine system prompt content, nudge strategy, judge behavior,
//!    goal tracking, cache usage, etc.
//!
//! 2. [`EvalHarness`] — plumbing set by the launching process (run_instance.sh,
//!    eval operator) for the child raven-tui process. Python paths, metrics
//!    output location, scenario identifiers. Only populated in eval/test runs.
//!
//! The split keeps "what the agent does" separate from "infrastructure the
//! harness needs to pass through."

use std::path::PathBuf;

// ─── Behavioral flags ────────────────────────────────────────────────────────

/// Controls how the agent behaves: system prompt sections, nudge/judge
/// strategy, goal tracking, cache workflow, and prompt construction.
///
/// Constructed once in `main.rs` from CLI flags + env vars, then stored
/// in `Config` and queried structurally everywhere.
#[derive(Clone, Debug, Default)]
pub struct RuntimeFlags {
    // ── Eval mode ──────────────────────────────────────────────────────
    /// True when running under the eval harness (RAVEN_EVAL, RAVEN_EVAL_MOCK_LLM,
    /// or --eval flag). Controls system prompt content, summary strategy,
    /// anti-rabbithole stripping, eval-specific goal seeding, and the
    /// synthetic user marker fallback.
    ///
    /// Replaces the ~6 inline `std::env::var("RAVEN_EVAL")` checks.
    pub is_eval: bool,

    // ── Goal tracking ──────────────────────────────────────────────────
    /// Enable goal tracking features (update_goal tool, goal section in
    /// injection block, goal seeding from first user request).
    ///
    /// Off by default in interactive mode. Enabled via RAVEN_GOAL_TRACKING=1
    /// or programmatically for scenarios that want it.
    ///
    /// Replaces ~6 inline `std::env::var("RAVEN_GOAL_TRACKING")` checks.
    pub goal_tracking: bool,

    /// Suppress the update_goal tool even when goal_tracking is on.
    /// Used in experiments that want goal display but not model-driven updates.
    ///
    /// Replaces RAVEN_EVAL_DISABLE_UPDATE_GOAL and RAVEN_NO_GOAL.
    pub disable_goal_tool: bool,

    /// Prevent auto-seeding an initial goal from the first user prompt.
    /// Used in experiments isolating goal-tracking behavior.
    ///
    /// Replaces RAVEN_EVAL_NO_INITIAL_GOAL + RAVEN_NO_GOAL (for the seeding aspect).
    pub no_initial_goal: bool,

    // ── Judge / nudge ──────────────────────────────────────────────────
    /// Enable the full V2 nudge/judge/criteria logic (define_done +
    /// budgeted progress continues, no hard budget cap on judge Continue).
    ///
    /// Previously --enable-judge CLI flag. Kept here so scenarios and
    /// profiles can set it too.
    pub enable_judge: bool,

    // ── Safety limits ──────────────────────────────────────────────────
    /// Hard wall-clock timeout for a single turn (seconds).
    /// When elapsed, drive_turn() stops the agent regardless of judge/nudge state.
    /// Set via --max-duration CLI, "max_duration" in scenario JSON, or default.
    /// None = no timeout (interactive default). Evals default to 600 (10 min).
    pub max_duration_secs: Option<u64>,

    // ── Presentation / terminal ────────────────────────────────────────
    /// Override terminal color depth (e.g. "256", "truecolor").
    /// Replaces RAVEN_COLOR_DEPTH.
    #[allow(dead_code)]
    pub color_depth: Option<String>,

    // ── Secrets ────────────────────────────────────────────────────────
    /// Vault password for encrypted keystore.
    /// From RAVEN_VAULT_PASSWORD env var (secrets never on CLI).
    pub vault_password: Option<String>,
}

impl RuntimeFlags {
    /// Resolve all runtime flags from environment variables.
    ///
    /// Call this **once** in `main.rs` at startup. The returned struct
    /// should be stored in `Config` and passed to all modules.
    ///
    /// CLI flags (e.g. --enable-judge) should be merged by the caller
    /// after this returns.
    pub fn from_env() -> Self {
        let is_eval =
            std::env::var("RAVEN_EVAL").is_ok() || std::env::var("RAVEN_EVAL_MOCK_LLM").is_ok();

        let goal_tracking = std::env::var("RAVEN_GOAL_TRACKING").is_ok();

        let disable_goal_tool = std::env::var("RAVEN_EVAL_DISABLE_UPDATE_GOAL").is_ok()
            || std::env::var("RAVEN_NO_GOAL").is_ok();

        let no_initial_goal = std::env::var("RAVEN_NO_GOAL").is_ok()
            || std::env::var("RAVEN_EVAL_NO_INITIAL_GOAL").is_ok();

        let enable_judge = false; // set by CLI --enable-judge; env never sets this

        let color_depth = std::env::var("RAVEN_COLOR_DEPTH").ok();
        let vault_password = std::env::var("RAVEN_VAULT_PASSWORD").ok();

        // max_duration: no env var — set by CLI or scenario JSON only.
        // Default for evals (is_eval) is applied in main.rs after merge.
        let max_duration_secs = None;

        Self {
            is_eval,
            goal_tracking,
            disable_goal_tool,
            no_initial_goal,
            enable_judge,
            color_depth,
            vault_password,
            max_duration_secs,
        }
    }
}

// ─── Eval harness plumbing ───────────────────────────────────────────────────

/// Infrastructure details set by the eval harness (run_instance.sh, eval
/// operator, or test scaffolding) for the child raven-tui process.
///
/// These are consumed in `main.rs` and a few eval-specific paths. They are
/// never behavioral switches — they are paths, identifiers, and output sinks.
#[derive(Clone, Debug, Default)]
pub struct EvalHarness {
    /// Full path to the project's Python interpreter (set by run_instance.sh).
    pub eval_python: Option<String>,
    /// Full path to python3 in the project venv.
    pub eval_python3: Option<String>,
    /// Path to the project venv directory.
    #[allow(dead_code)]
    pub eval_project_venv: Option<String>,
    /// Where to write harness turn metrics (JSON).
    pub metrics_out: Option<PathBuf>,
    /// Override workspace for eval (used by eval_smoke test harness).
    #[allow(dead_code)]
    pub eval_workspace: Option<PathBuf>,
    /// Which scenario to run (used by eval_smoke test harness).
    #[allow(dead_code)]
    pub eval_scenario: Option<String>,
    /// Initial prompt file (used by TUI eval mode).
    pub initial_prompt_file: Option<PathBuf>,
    /// Strict assertion mode for eval metrics.
    pub assert_strict: bool,
}

impl EvalHarness {
    /// Resolve all eval harness plumbing from environment variables.
    ///
    /// Call this **once** in `main.rs` at startup. Returns Default if
    /// no eval env vars are set (normal interactive use).
    pub fn from_env() -> Self {
        Self {
            eval_python: std::env::var("RAVEN_EVAL_PYTHON").ok(),
            eval_python3: std::env::var("RAVEN_EVAL_PYTHON3").ok(),
            eval_project_venv: std::env::var("RAVEN_EVAL_PROJECT_VENV").ok(),
            metrics_out: std::env::var("RAVEN_METRICS_OUT").ok().map(PathBuf::from),
            eval_workspace: std::env::var("RAVEN_EVAL_WORKSPACE")
                .ok()
                .map(PathBuf::from),
            eval_scenario: std::env::var("RAVEN_EVAL_SCENARIO").ok(),
            initial_prompt_file: std::env::var("RAVEN_EVAL_INITIAL_PROMPT_FILE")
                .ok()
                .map(PathBuf::from),
            assert_strict: std::env::var("RAVEN_EVAL_ASSERT_STRICT")
                .ok()
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_flags_are_all_off() {
        let f = RuntimeFlags::default();
        assert!(!f.is_eval);
        assert!(!f.goal_tracking);
        assert!(!f.disable_goal_tool);
        assert!(!f.no_initial_goal);
        assert!(!f.enable_judge);
        assert!(f.color_depth.is_none());
        assert!(f.vault_password.is_none());
    }

    #[test]
    fn default_harness_is_empty() {
        let h = EvalHarness::default();
        assert!(h.eval_python.is_none());
        assert!(h.metrics_out.is_none());
        assert!(!h.assert_strict);
    }
}
