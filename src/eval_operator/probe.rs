//! Lightweight LLM reachability probe for the eval operator.

use reqwest::blocking::Client;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct LlmStatus {
    pub base_url: String,
    pub reachable: bool,
    pub model_hint: Option<String>,
    pub context_tokens: Option<u32>,
    pub error: Option<String>,
}

pub fn probe_llm(base_url: &str) -> LlmStatus {
    let base = base_url.trim_end_matches('/');
    let url = format!("{base}/models");
    let client = match Client::builder().timeout(Duration::from_secs(5)).build() {
        Ok(c) => c,
        Err(e) => {
            return LlmStatus {
                base_url: base_url.to_string(),
                reachable: false,
                model_hint: None,
                context_tokens: None,
                error: Some(e.to_string()),
            };
        }
    };

    match client.get(&url).send() {
        Ok(resp) if resp.status().is_success() => {
            let model_hint = resp.json::<serde_json::Value>().ok().and_then(|body| {
                body.get("data")
                    .and_then(|d| d.as_array())
                    .and_then(|a| a.first())
                    .and_then(|m| m.get("id").and_then(|v| v.as_str()))
                    .map(|s| s.to_string())
            });
            let context_tokens = None; // operator only needs reachability for v1
            LlmStatus {
                base_url: base_url.to_string(),
                reachable: true,
                model_hint,
                context_tokens,
                error: None,
            }
        }
        Ok(resp) => LlmStatus {
            base_url: base_url.to_string(),
            reachable: false,
            model_hint: None,
            context_tokens: None,
            error: Some(format!("HTTP {}", resp.status())),
        },
        Err(e) => LlmStatus {
            base_url: base_url.to_string(),
            reachable: false,
            model_hint: None,
            context_tokens: None,
            error: Some(e.to_string()),
        },
    }
}

pub fn default_base_url() -> String {
    std::env::var("LLM_BASE_URL").unwrap_or_else(|_| "http://127.0.0.1:8080/v1".into())
}

pub fn format_status(s: &LlmStatus) -> String {
    if s.reachable {
        let model = s
            .model_hint
            .as_deref()
            .unwrap_or("(model unknown)");
        format!("LLM: {}  ✓ reachable ({model})", s.base_url)
    } else {
        let err = s.error.as_deref().unwrap_or("unreachable");
        format!("LLM: {}  ✗ {err}", s.base_url)
    }
}