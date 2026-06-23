//! Shared `/v1/models` probing for TUI, replay evals, and `raven-eval`.
//!
//! Resolves model id + context window from llama.cpp, OpenRouter, and similar
//! OpenAI-compatible servers.

use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;

/// How a model id was chosen during `/v1/models` probing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeMatch {
    Exact,
    Alias,
    CaseInsensitive,
    /// Server exposes exactly one model.
    SingleModel,
    /// Multiple models; picked the first entry with a known context size.
    FirstWithContext,
}

impl ProbeMatch {
    pub fn as_str(self) -> &'static str {
        match self {
            ProbeMatch::Exact => "exact",
            ProbeMatch::Alias => "alias",
            ProbeMatch::CaseInsensitive => "case_insensitive",
            ProbeMatch::SingleModel => "single_model",
            ProbeMatch::FirstWithContext => "first_with_context",
        }
    }
}

/// Result of probing an OpenAI-compatible server's `/v1/models` endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerProbeResult {
    pub model_id: String,
    pub context_tokens: u32,
    pub matched_by: ProbeMatch,
}

fn model_aliases(m: &Value) -> Vec<&str> {
    m.get("aliases")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default()
}

/// Extract context window from a single model entry (llama.cpp or OpenRouter shape).
pub fn extract_context_tokens(m: &Value) -> Option<u32> {
    if let Some(n) = m.get("context_length").and_then(|v| v.as_u64()) {
        return Some(n as u32);
    }
    if let Some(n) = m
        .get("meta")
        .and_then(|meta| meta.get("n_ctx"))
        .and_then(|v| v.as_u64())
    {
        return Some(n as u32);
    }
    None
}

fn probe_result_from_entry(m: &Value, matched_by: ProbeMatch) -> Option<ServerProbeResult> {
    let id = m.get("id").and_then(|v| v.as_str())?.to_string();
    let context_tokens = extract_context_tokens(m)?;
    Some(ServerProbeResult {
        model_id: id,
        context_tokens,
        matched_by,
    })
}

/// Resolve model id + context from a `/v1/models` JSON body.
///
/// Matching order when `model_hint` is non-empty:
///   1. exact `id`
///   2. `aliases`
///   3. case-insensitive `id`
///
/// When no hint matches (or hint is empty):
///   4. sole model in `data`
///   5. first model with a known context size
pub fn resolve_server_probe(body: &Value, model_hint: &str) -> Option<ServerProbeResult> {
    let data = body.get("data")?.as_array()?;
    if data.is_empty() {
        return None;
    }

    let hint = model_hint.trim();

    if !hint.is_empty() {
        for m in data {
            let id = m.get("id").and_then(|v| v.as_str()).unwrap_or("");
            if id == hint {
                if let Some(r) = probe_result_from_entry(m, ProbeMatch::Exact) {
                    return Some(r);
                }
            }
            if model_aliases(m).contains(&hint) {
                if let Some(r) = probe_result_from_entry(m, ProbeMatch::Alias) {
                    return Some(r);
                }
            }
        }

        let hint_lower = hint.to_lowercase();
        for m in data {
            let id = m.get("id").and_then(|v| v.as_str()).unwrap_or("");
            if id.to_lowercase() == hint_lower {
                if let Some(r) = probe_result_from_entry(m, ProbeMatch::CaseInsensitive) {
                    return Some(r);
                }
            }
        }
    }

    if data.len() == 1 {
        return probe_result_from_entry(&data[0], ProbeMatch::SingleModel);
    }

    for m in data {
        if let Some(r) = probe_result_from_entry(m, ProbeMatch::FirstWithContext) {
            return Some(r);
        }
    }

    None
}

fn fetch_models_body(
    base_url: &str,
    api_key: Option<&str>,
) -> Result<Value, String> {
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?;

    let mut req = client.get(&url);
    if let Some(key) = api_key.filter(|k| !k.is_empty()) {
        req = req.bearer_auth(key);
    }

    let resp = req.send().map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    resp.json::<Value>().map_err(|e| e.to_string())
}

/// Blocking probe for `raven-eval` and other sync callers.
pub fn probe_server_blocking(
    base_url: &str,
    model_hint: &str,
    api_key: Option<&str>,
) -> Result<ServerProbeResult, String> {
    let body = fetch_models_body(base_url, api_key)?;
    resolve_server_probe(&body, model_hint)
        .ok_or_else(|| "could not resolve model id and context from /v1/models".into())
}

/// Probe model id and context window via `/v1/models` (async).
pub async fn probe_server(
    base_url: &str,
    model_hint: &str,
    api_key: Option<&str>,
) -> Option<ServerProbeResult> {
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let client = Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .ok()?;

    let mut req = client.get(&url);
    if let Some(key) = api_key.filter(|k| !k.is_empty()) {
        req = req.bearer_auth(key);
    }

    let resp = req.send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body: Value = resp.json().await.ok()?;
    resolve_server_probe(&body, model_hint)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn resolve_probe_llama_cpp_exact_match() {
        let body = json!({
            "data": [{
                "id": "Qwen3-Coder-Next",
                "meta": { "n_ctx": 65536 }
            }]
        });
        let r = resolve_server_probe(&body, "Qwen3-Coder-Next").unwrap();
        assert_eq!(r.model_id, "Qwen3-Coder-Next");
        assert_eq!(r.context_tokens, 65536);
        assert_eq!(r.matched_by, ProbeMatch::Exact);
    }

    #[test]
    fn resolve_probe_single_model_without_hint() {
        let body = json!({
            "data": [{
                "id": "Qwen3-Coder-Next",
                "meta": { "n_ctx": 65536 }
            }]
        });
        let r = resolve_server_probe(&body, "").unwrap();
        assert_eq!(r.model_id, "Qwen3-Coder-Next");
        assert_eq!(r.matched_by, ProbeMatch::SingleModel);
    }

    #[test]
    fn resolve_probe_falls_back_when_configured_name_wrong() {
        let body = json!({
            "data": [{
                "id": "Qwen3-Coder-Next",
                "meta": { "n_ctx": 65536 }
            }]
        });
        let r = resolve_server_probe(&body, "qwen2.5-coder").unwrap();
        assert_eq!(r.model_id, "Qwen3-Coder-Next");
        assert_eq!(r.matched_by, ProbeMatch::SingleModel);
    }

    #[test]
    fn resolve_probe_case_insensitive() {
        let body = json!({
            "data": [{
                "id": "Qwen3-Coder-Next",
                "meta": { "n_ctx": 32768 }
            }]
        });
        let r = resolve_server_probe(&body, "qwen3-coder-next").unwrap();
        assert_eq!(r.matched_by, ProbeMatch::CaseInsensitive);
    }

    #[test]
    fn resolve_probe_openrouter_context_length() {
        let body = json!({
            "data": [{
                "id": "anthropic/claude-sonnet-4",
                "context_length": 200000
            }]
        });
        let r = resolve_server_probe(&body, "anthropic/claude-sonnet-4").unwrap();
        assert_eq!(r.context_tokens, 200000);
    }

    #[test]
    fn resolve_probe_llama_cpp_hybrid_models_and_data() {
        let body = json!({
            "models": [{
                "name": "qwen3-coder-next",
                "model": "qwen3-coder-next",
            }],
            "data": [{
                "id": "qwen3-coder-next",
                "aliases": ["qwen3-coder-next"],
                "meta": { "n_ctx": 65536 }
            }]
        });

        let exact = resolve_server_probe(&body, "qwen3-coder-next").unwrap();
        assert_eq!(exact.matched_by, ProbeMatch::Exact);

        let fallback = resolve_server_probe(&body, "qwen2.5-coder").unwrap();
        assert_eq!(fallback.model_id, "qwen3-coder-next");
        assert_eq!(fallback.matched_by, ProbeMatch::SingleModel);
    }
}