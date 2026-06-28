use std::path::PathBuf;

use crate::runtime::{EvalHarness, RuntimeFlags};

/// Computed budget limits derived from the server's actual context window size.
/// All tool-result truncation limits, file-read line caps, etc. flow from this
/// so the agent automatically adapts to any model / backend.
#[derive(Clone, Debug)]
pub struct ContextBudget {
    /// Raw context size in tokens (from server probe or CLI override)
    pub context_tokens: u32,
    /// Max bytes per tool result before truncation
    pub tool_result_bytes: usize,
    /// Max lines for a default (no range) file read
    pub read_line_limit: usize,
    /// How the value was obtained
    pub source: ContextSource,
}

#[derive(Clone, Debug)]
pub enum ContextSource {
    Probed,
    CliOverride,
    Default,
}

impl std::fmt::Display for ContextSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Probed => write!(f, "probed"),
            Self::CliOverride => write!(f, "override"),
            Self::Default => write!(f, "default"),
        }
    }
}

impl ContextBudget {
    /// Compute budget from context window size and desired tool rounds.
    ///
    /// Budget allocation (% of estimated context bytes):
    ///   system + session:   10%
    ///   conversation:       20%
    ///   tool results:       60%  ← the lever we size
    ///   response reserve:   10%
    pub fn from_context_tokens(n_ctx: u32, desired_rounds: u32) -> Self {
        let bytes_per_token: f64 = 3.5; // conservative average for code
        let context_bytes = n_ctx as f64 * bytes_per_token;
        let tool_budget = context_bytes * 0.60;
        let rounds = (desired_rounds as f64).max(1.0);

        let tool_result_bytes = ((tool_budget / rounds) as usize).clamp(500, 50_000);

        let read_line_limit = (tool_result_bytes / 45).clamp(20, 1_000);

        Self {
            context_tokens: n_ctx,
            tool_result_bytes,
            read_line_limit,
            source: ContextSource::Probed,
        }
    }

    /// Safe fallback when probing fails and no CLI override is given.
    pub fn default_fallback() -> Self {
        let mut b = Self::from_context_tokens(8192, 10);
        b.source = ContextSource::Default;
        b
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
    pub workspace: PathBuf,
    pub temperature: f32,
    pub max_tokens: u32,
    pub max_rounds: u32,
    /// Optional pre-initialized session from main (for trust prompt + repo cache bootstrap).
    /// If present, Agent will use it instead of doing its own Session::init.
    pub prebuilt_session: Option<crate::session::Session>,
    /// Computed context budget — derived from server probe, CLI override, or default.
    pub context_budget: ContextBudget,
    /// Real tools vs scripted eval mocks (`RAVEN_EVAL=1`).
    pub tool_backend: crate::tools::ToolBackend,
    /// When false, tool schemas are omitted from LLM requests (connectivity-only evals).
    pub tools_enabled: bool,
    /// Enable the full V2 nudge/judge/criteria logic (define_done + progress-based continues, no hard budget cap on judge Continue).
    pub enable_judge: bool,
    /// Centralized behavioral flags — replaces all inline `std::env::var("RAVEN_*")` checks.
    pub flags: RuntimeFlags,
    /// Eval harness plumbing (python paths, metrics output, etc.). Empty in normal use.
    pub harness: EvalHarness,
}

impl Config {
    pub fn chat_url(&self) -> String {
        let base = self.base_url.trim_end_matches('/');
        format!("{}/chat/completions", base)
    }

    /// Convenience: are we running under the eval harness?
    #[allow(dead_code)]
    pub fn is_eval(&self) -> bool {
        self.flags.is_eval
    }
}

/// Runtime representation of an inference endpoint (API key decrypted in memory).
#[derive(Clone, Debug)]
pub struct InferenceEndpoint {
    pub label: String,
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>, // decrypted, in-memory only
}

impl InferenceEndpoint {
    /// Build from the CLI/config defaults (the always-present "local" endpoint).
    pub fn from_config(config: &Config) -> Self {
        Self {
            label: format!("CLI ({})", config.model),
            base_url: config.base_url.clone(),
            model: config.model.clone(),
            api_key: config.api_key.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_context_budget_bounds() {
        let b = ContextBudget::from_context_tokens(8192, 10);
        assert!(b.tool_result_bytes >= 500);
        assert!(b.tool_result_bytes <= 50_000);
        assert!(b.read_line_limit >= 20);
        assert!(b.read_line_limit <= 1000);

        let small = ContextBudget::from_context_tokens(1024, 1);
        assert!(small.tool_result_bytes >= 500);
    }
}
