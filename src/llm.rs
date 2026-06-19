//! OpenAI-compatible chat completions client with first-class tool calling support.
//!
//! Designed to work well with llama.cpp server (and other local OpenAI-compatible servers).
//! Handles both non-streaming and streaming responses, including accumulation of
//! partial tool_calls during streaming (the format used by most servers).

use anyhow::{anyhow, Result};
use bytes::Bytes;
use futures_util::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::mpsc;

use crate::config::Config;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub r#type: String, // "function"
    pub function: FunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String, // JSON string
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolDef {
    #[serde(rename = "type")]
    pub r#type: String, // "function"
    pub function: ToolFunction,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolFunction {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value, // JSON Schema object
}

#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub messages: Vec<Message>,
    pub tools: Option<Vec<ToolDef>>,
    pub temperature: f32,
    pub max_tokens: u32,
    pub stream: bool,
}

#[derive(Debug, Clone)]
pub struct ChatResponse {
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
    pub finish_reason: Option<String>,
    /// Raw usage if provided by the server
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: Option<u32>,
    pub completion_tokens: Option<u32>,
    pub total_tokens: Option<u32>,
}

/// Streaming chunk types emitted by the client.
#[derive(Debug, Clone)]
pub enum StreamChunk {
    Token(String),
    Thinking(String), // for models that emit reasoning_content (Qwen etc.)
    Done { content: String, tool_calls: Vec<ToolCall>, usage: Option<Usage> },
    Error(String),
}

/// Low-level OpenAI compatible client.
pub struct LlmClient {
    http: Client,
    config: Config,
}

impl LlmClient {
    pub fn new(config: Config) -> Self {
        let http = Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .build()
            .expect("failed to build http client");

        Self { http, config }
    }

    pub async fn chat(&self, req: ChatRequest) -> Result<ChatResponse> {
        let mut body = json!({
            "model": self.config.model,
            "messages": req.messages,
            "temperature": req.temperature,
            "max_tokens": req.max_tokens,
            "stream": false,
        });

        if let Some(tools) = &req.tools {
            body["tools"] = serde_json::to_value(tools)?;
        }

        // Enable reasoning/thinking for providers that support it (OpenRouter, etc.)
        if is_openrouter(&self.config.base_url) {
            body["reasoning"] = json!({ "enabled": true });
        }

        let mut request = self.http.post(self.config.chat_url())
            .json(&body);

        if let Some(key) = &self.config.api_key {
            request = request.bearer_auth(key);
        }

        // OpenRouter attribution headers
        if is_openrouter(&self.config.base_url) {
            request = request
                .header("HTTP-Referer", "https://github.com/raven-hotel")
                .header("X-Title", "Raven Hotel TUI");
        }

        let resp = request.send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("LLM error {}: {}", status, text));
        }

        let data: serde_json::Value = resp.json().await?;

        let choice = data
            .get("choices")
            .and_then(|c| c.get(0))
            .ok_or_else(|| anyhow!("no choices in response"))
            .cloned()
            .unwrap_or_else(|_| json!({}));

        let message = choice.get("message").cloned().unwrap_or_else(|| json!({}));
        let content = message
            .get("content")
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .to_string();

        let tool_calls = parse_tool_calls(message.get("tool_calls"));

        let usage = data.get("usage").and_then(|u| {
            serde_json::from_value::<Usage>(u.clone()).ok()
        });

        Ok(ChatResponse {
            content,
            tool_calls,
            finish_reason: choice
                .get("finish_reason")
                .and_then(|f| f.as_str())
                .map(|s| s.to_string()),
            usage,
        })
    }

    /// Streaming chat. Sends tokens and final tool calls over the channel.
    /// Returns the sender side; caller should consume the receiver.
    pub async fn chat_stream(
        &self,
        req: ChatRequest,
    ) -> Result<mpsc::Receiver<StreamChunk>> {
        let (tx, rx) = mpsc::channel(128);

        let mut body = json!({
            "model": self.config.model,
            "messages": req.messages,
            "temperature": req.temperature,
            "max_tokens": req.max_tokens,
            "stream": true,
        });

        if let Some(tools) = &req.tools {
            // Standard OpenAI tool format that llama.cpp and most servers understand
            body["tools"] = serde_json::to_value(tools)?;
        }

        // Enable reasoning/thinking for providers that support it
        let openrouter = is_openrouter(&self.config.base_url);
        if openrouter {
            body["reasoning"] = json!({ "enabled": true });
        }

        // Spawn the actual streaming work so the caller can immediately get the receiver
        let http = self.http.clone();
        let url = self.config.chat_url();
        let api_key = self.config.api_key.clone();

        tokio::spawn(async move {
            let _ = Self::do_stream(http, url, api_key, body, tx, openrouter).await;
        });

        Ok(rx)
    }

    async fn do_stream(
        http: Client,
        url: String,
        api_key: Option<String>,
        body: serde_json::Value,
        tx: mpsc::Sender<StreamChunk>,
        openrouter: bool,
    ) -> Result<()> {
        let mut req = http.post(url).json(&body);
        if let Some(k) = api_key {
            req = req.bearer_auth(k);
        }
        if openrouter {
            req = req
                .header("HTTP-Referer", "https://github.com/raven-hotel")
                .header("X-Title", "Raven Hotel TUI");
        }

        let resp = req.send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            let _ = tx.send(StreamChunk::Error(format!("{}: {}", status, text))).await;
            return Ok(());
        }

        let mut full_text = String::new();
        let mut tool_accum: HashMap<usize, ToolCall> = HashMap::new();
        let mut last_usage: Option<Usage> = None;
        let mut stream = resp.bytes_stream();
        let mut buffer = String::new();

        while let Some(chunk_result) = stream.next().await {
            let bytes: Bytes = match chunk_result {
                Ok(b) => b,
                Err(e) => {
                    let _ = tx.send(StreamChunk::Error(e.to_string())).await;
                    break;
                }
            };

            buffer.push_str(&String::from_utf8_lossy(&bytes));

            // Process complete SSE lines
            while let Some(line_end) = buffer.find('\n') {
                let line = buffer[..line_end].trim().to_string();
                buffer = buffer[line_end + 1..].to_string();

                if line.is_empty() || !line.starts_with("data: ") {
                    continue;
                }

                let data_str = &line[6..];
                if data_str == "[DONE]" {
                    // Emit final
                    let final_tools: Vec<ToolCall> = tool_accum
                        .into_values()
                        .collect();

                    let _ = tx
                        .send(StreamChunk::Done {
                            content: full_text.clone(),
                            tool_calls: final_tools.clone(),
                            usage: last_usage,
                        })
                        .await;
                    return Ok(());
                }

                if let Ok(json) = serde_json::from_str::<serde_json::Value>(data_str) {
                    // Some servers send usage as a separate object
                    if let Some(usage_val) = json.get("usage") {
                        if let Ok(u) = serde_json::from_value::<Usage>(usage_val.clone()) {
                            last_usage = Some(u);
                        }
                    }

                    if let Some(choices) = json.get("choices").and_then(|c| c.as_array()) {
                        if let Some(choice) = choices.first() {
                            let delta_val = choice.get("delta").cloned().unwrap_or_else(|| json!({}));
                            let delta = &delta_val;

                            // Reasoning / thinking content (Qwen-style: reasoning_content,
                            // OpenRouter/Kimi-style: reasoning)
                            if let Some(thinking) = delta.get("reasoning_content")
                                .and_then(|v| v.as_str())
                                .or_else(|| delta.get("reasoning").and_then(|v| v.as_str()))
                            {
                                if !thinking.is_empty() {
                                    let _ = tx.send(StreamChunk::Thinking(thinking.to_string())).await;
                                }
                            }

                            // Regular content
                            if let Some(token) = delta.get("content").and_then(|v| v.as_str()) {
                                if !token.is_empty() {
                                    full_text.push_str(token);
                                    let _ = tx.send(StreamChunk::Token(token.to_string())).await;
                                }
                            }

                            // Tool call deltas — accumulate by index (same logic as Raven's inference_router)
                            if let Some(tool_deltas) = delta.get("tool_calls").and_then(|t| t.as_array()) {
                                for td in tool_deltas {
                                    let idx = td.get("index")
                                        .and_then(|i| i.as_u64())
                                        .unwrap_or(0) as usize;

                                    let entry = tool_accum.entry(idx).or_insert_with(|| ToolCall {
                                        id: String::new(),
                                        r#type: "function".to_string(),
                                        function: FunctionCall {
                                            name: String::new(),
                                            arguments: String::new(),
                                        },
                                    });

                                    if let Some(id) = td.get("id").and_then(|v| v.as_str()) {
                                        if !id.is_empty() {
                                            entry.id = id.to_string();
                                        }
                                    }

                                    if let Some(func) = td.get("function") {
                                        if let Some(name) = func.get("name").and_then(|v| v.as_str()) {
                                            if !name.is_empty() {
                                                entry.function.name = name.to_string();
                                            }
                                        }
                                        if let Some(args) = func.get("arguments").and_then(|v| v.as_str()) {
                                            entry.function.arguments.push_str(args);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // If we fell out without a [DONE], still emit what we have
        let final_tools: Vec<ToolCall> = tool_accum.into_values().collect();
        let _ = tx
            .send(StreamChunk::Done {
                content: full_text,
                tool_calls: final_tools,
                usage: last_usage,
            })
            .await;

        Ok(())
    }
}

fn parse_tool_calls(value: Option<&serde_json::Value>) -> Vec<ToolCall> {
    let Some(arr) = value.and_then(|v| v.as_array()) else {
        return vec![];
    };

    let mut out = vec![];
    for item in arr {
        if let Ok(tc) = serde_json::from_value::<ToolCall>(item.clone()) {
            out.push(tc);
        } else if let Some(func) = item.get("function") {
            // Very defensive parsing for slightly non-standard servers
            let id = item
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let name = func
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let arguments = func
                .get("arguments")
                .and_then(|v| v.as_str())
                .unwrap_or("{}")
                .to_string();

            out.push(ToolCall {
                id,
                r#type: "function".into(),
                function: FunctionCall { name, arguments },
            });
        }
    }
    out
}

/// Probe the server's context window size via the standard `/v1/models` endpoint.
///
/// Works with:
///   - **llama.cpp**: `data[].meta.n_ctx`
///   - **OpenRouter**: `data[].context_length`
///   - **Any server** that puts context size in either of those fields
///
/// Returns `None` on any failure (timeout, parse error, model not found, etc.)
/// so the caller can fall back to a CLI override or default.
pub async fn probe_context_size(base_url: &str, model: &str, api_key: Option<&str>) -> Option<u32> {
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let client = Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .ok()?;

    let mut req = client.get(&url);
    if let Some(key) = api_key {
        req = req.bearer_auth(key);
    }

    let resp = req.send().await.ok()?;
    let body: serde_json::Value = resp.json().await.ok()?;

    // Search the data array for the requested model
    for m in body.get("data")?.as_array()? {
        let id = m.get("id").and_then(|v| v.as_str()).unwrap_or("");
        // Match by exact id or by alias
        let aliases: Vec<&str> = m.get("aliases")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        if id != model && !aliases.contains(&model) {
            continue;
        }

        // OpenRouter / generic: context_length at top level
        if let Some(n) = m.get("context_length").and_then(|v| v.as_u64()) {
            return Some(n as u32);
        }
        // llama.cpp: meta.n_ctx
        if let Some(n) = m.get("meta").and_then(|meta| meta.get("n_ctx")).and_then(|v| v.as_u64()) {
            return Some(n as u32);
        }
    }

    None
}

/// Detect whether a base URL points to OpenRouter.
fn is_openrouter(base_url: &str) -> bool {
    base_url.contains("openrouter.ai")
}
