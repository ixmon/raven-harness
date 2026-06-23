//! LLM endpoint probe for `raven-eval` — same logic as the TUI (`server_probe`).

use crate::server_probe::{probe_server_blocking, ServerProbeResult};
use serde::Serialize;
use std::path::Path;

#[derive(Debug, Clone, Serialize)]
pub struct LlmStatus {
    pub base_url: String,
    pub model_hint: String,
    pub reachable: bool,
    pub model_id: Option<String>,
    pub context_tokens: Option<u32>,
    pub matched_by: Option<String>,
    /// Ready for live agent evals (reachable + resolved model + context).
    pub ready_for_agent: bool,
    pub error: Option<String>,
}

impl LlmStatus {
    pub fn from_probe(
        base_url: &str,
        model_hint: &str,
        result: Result<ServerProbeResult, String>,
    ) -> Self {
        match result {
            Ok(probe) => Self {
                base_url: base_url.to_string(),
                model_hint: model_hint.to_string(),
                reachable: true,
                model_id: Some(probe.model_id),
                context_tokens: Some(probe.context_tokens),
                matched_by: Some(probe.matched_by.as_str().to_string()),
                ready_for_agent: true,
                error: None,
            },
            Err(e) => Self {
                base_url: base_url.to_string(),
                model_hint: model_hint.to_string(),
                reachable: false,
                model_id: None,
                context_tokens: None,
                matched_by: None,
                ready_for_agent: false,
                error: Some(e),
            },
        }
    }

    pub fn write_json(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let payload = serde_json::json!({
            "version": 1,
            "base_url": self.base_url,
            "model_hint": self.model_hint,
            "reachable": self.reachable,
            "model_id": self.model_id,
            "context_tokens": self.context_tokens,
            "matched_by": self.matched_by,
            "ready_for_agent": self.ready_for_agent,
            "error": self.error,
        });
        std::fs::write(path, serde_json::to_string_pretty(&payload)?)
    }
}

pub fn model_hint_from_env() -> String {
    std::env::var("LLM_MODEL").unwrap_or_default()
}

pub fn probe_llm(base_url: &str) -> LlmStatus {
    let hint = model_hint_from_env();
    let api_key = std::env::var("LLM_API_KEY")
        .ok()
        .filter(|k| !k.is_empty());
    let result = probe_server_blocking(base_url, &hint, api_key.as_deref());
    LlmStatus::from_probe(base_url, &hint, result)
}

pub fn preflight_swebench_live(status: &LlmStatus) -> anyhow::Result<()> {
    if status.ready_for_agent {
        return Ok(());
    }
    if let Some(err) = &status.error {
        anyhow::bail!(
            "SWE-bench live preflight failed for {}: {err}",
            status.base_url
        );
    }
    anyhow::bail!(
        "SWE-bench live preflight failed: could not resolve model/context from {}/models (hint: {:?})",
        status.base_url.trim_end_matches('/'),
        if status.model_hint.is_empty() {
            "(none — set LLM_MODEL or use a single-model server)"
        } else {
            status.model_hint.as_str()
        }
    );
}

pub fn default_base_url() -> String {
    std::env::var("LLM_BASE_URL").unwrap_or_else(|_| "http://127.0.0.1:8080/v1".into())
}

pub fn format_status(s: &LlmStatus) -> String {
    if s.ready_for_agent {
        let model = s.model_id.as_deref().unwrap_or("?");
        let ctx = s
            .context_tokens
            .map(|n| n.to_string())
            .unwrap_or_else(|| "?".into());
        let matched = s.matched_by.as_deref().unwrap_or("?");
        format!(
            "LLM: {}  ✓ {} ({} ctx, matched_by={})",
            s.base_url, model, ctx, matched
        )
    } else if s.reachable {
        let err = s.error.as_deref().unwrap_or("unresolved model/context");
        format!("LLM: {}  ⚠ reachable but not ready — {err}", s.base_url)
    } else {
        let err = s.error.as_deref().unwrap_or("unreachable");
        format!("LLM: {}  ✗ {err}", s.base_url)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server_probe::ProbeMatch;
    use serde_json::json;

    #[test]
    fn from_probe_ok() {
        let body = json!({
            "data": [{
                "id": "qwen3-coder-next",
                "meta": { "n_ctx": 65536 }
            }]
        });
        let probe = crate::server_probe::resolve_server_probe(&body, "qwen2.5-coder").unwrap();
        let status = LlmStatus::from_probe("http://127.0.0.1:8080/v1", "qwen2.5-coder", Ok(probe));
        assert!(status.ready_for_agent);
        assert_eq!(status.model_id.as_deref(), Some("qwen3-coder-next"));
        assert_eq!(status.matched_by.as_deref(), Some(ProbeMatch::SingleModel.as_str()));
    }
}