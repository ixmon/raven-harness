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

#[allow(unused_imports)]
pub use crate::server_probe::{extract_context_tokens, resolve_server_probe, ProbeMatch, ServerProbeResult};
pub use crate::server_probe::probe_server;

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
    #[allow(dead_code)]
    pub stream: bool,
}

#[derive(Debug, Clone)]
pub struct ChatResponse {
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
    #[allow(dead_code)]
    pub finish_reason: Option<String>,
    /// Raw usage if provided by the server
    #[allow(dead_code)]
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    Done {
        content: String,
        tool_calls: Vec<ToolCall>,
        #[allow(dead_code)]
        usage: Option<Usage>,
        /// "stop", "length", "tool_calls", etc. — needed for nudge decisions.
        finish_reason: Option<String>,
    },
    Error(String),
}

/// Low-level OpenAI compatible client.
#[derive(Clone)]
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
        let original_content = assistant_text_from_message(&message);

        let mut tool_calls = parse_tool_calls(message.get("tool_calls"));
        if tool_calls.is_empty() {
            tool_calls = parse_xml_tool_calls_from_content(&original_content);
        }

        // Always strip any tool XML fragments (including stray closing tags like
        // "}</parameter></function></tool_call>") from the content we expose.
        // This prevents junk from being pushed as assistant text or shown as output
        // even when the XML parser didn't recognize a full call.
        let content = strip_xml_tool_call_blocks(&original_content);

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
        let mut last_finish_reason: Option<String> = None;
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
                    let mut final_tools: Vec<ToolCall> = tool_accum
                        .into_values()
                        .collect();

                    // Fallback: if no structured tool_calls were accumulated
                    // but the content contains XML tool calls (Qwen native format),
                    // parse them from the text.
                    if final_tools.is_empty() {
                        final_tools = parse_xml_tool_calls_from_content(&full_text);
                    }

                    // Always strip tool XML syntax so that even if parse didn't extract
                    // calls (e.g. only stray closing tags were emitted), we don't return
                    // or display "}</parameter>..." etc. as agent text.
                    let sent_content = strip_xml_tool_call_blocks(&full_text);

                    let _ = tx
                        .send(StreamChunk::Done {
                            content: sent_content,
                            tool_calls: final_tools.clone(),
                            usage: last_usage,
                            finish_reason: last_finish_reason,
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
                            // Capture finish_reason from the last chunk that has it
                            if let Some(fr) = choice.get("finish_reason").and_then(|v| v.as_str()) {
                                last_finish_reason = Some(fr.to_string());
                            }
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
                                    // Suppress forwarding of tokens that are part of XML tool call syntax
                                    // (the model is emitting the call format as content text). We still
                                    // accumulate into full_text for parse_xml, and the final Done will
                                    // contain a cleaned version. This prevents the UI/history from
                                    // showing raw "}<function=..." , "function=..." or stray closing
                                    // tags like "}</parameter></function></tool_call>".
                                    let token_has_xml = contains_tool_xml_syntax(token);
                                    let entering_tool_xml = token_has_xml || contains_tool_xml_syntax(&full_text);
                                    if !entering_tool_xml {
                                        let _ = tx.send(StreamChunk::Token(token.to_string())).await;
                                    }
                                }
                            }

                            if let Some(tool_deltas) = delta.get("tool_calls").and_then(|t| t.as_array()) {
                                accumulate_tool_call_deltas(&mut tool_accum, tool_deltas);
                            }
                        }
                    }
                }
            }
        }

        // If we fell out without a [DONE], still emit what we have
        let mut final_tools: Vec<ToolCall> = tool_accum.into_values().collect();
        // XML fallback for Qwen-style tool calls in content
        if final_tools.is_empty() {
            final_tools = parse_xml_tool_calls_from_content(&full_text);
        }

        // Always strip to avoid leaking partial XML (including pure closers) into round_text.
        let sent_content = strip_xml_tool_call_blocks(&full_text);

        let _ = tx
            .send(StreamChunk::Done {
                content: sent_content,
                tool_calls: final_tools,
                usage: last_usage,
                finish_reason: last_finish_reason,
            })
            .await;

        Ok(())
    }
}

/// Prefer `content`; fall back to `reasoning_content` (Qwen / llama.cpp thinking).
fn assistant_text_from_message(message: &serde_json::Value) -> String {
    let content = message
        .get("content")
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .trim();
    if !content.is_empty() {
        return content.to_string();
    }
    message
        .get("reasoning_content")
        .and_then(|c| c.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("")
        .to_string()
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

/// Qwen / some llama.cpp templates emit tool calls as XML in assistant text.
///
/// Handles two variants:
/// 1. Full: `<tool_call>\n<function=name>\n<parameter=key>value</parameter>\n</function>\n</tool_call>`
/// 2. Truncated (llama.cpp streaming often strips the leading `<tool_call>\n<`):
///    `function=name>\n<parameter=key>value</parameter>\n</function>\n</tool_call>`
fn parse_xml_tool_calls_from_content(content: &str) -> Vec<ToolCall> {
    // Quick reject: need either `<function=` or bare `function=` or parameter fragments
    let has_xml_function = content.contains("<function=") || content.contains("<function>");
    let has_bare_function = content.contains("function=") || content.contains("function>");
    let has_parameter = content.contains("<parameter") || content.contains("parameter=") || content.contains("parameter ");
    if !has_xml_function && !has_bare_function && !has_parameter {
        return vec![];
    }

    // Normalize: if the content starts with the truncated form (missing `<`),
    // prepend `<` so the rest of the parser sees `<function=`.
    let normalized: std::borrow::Cow<str> = if !has_xml_function && has_bare_function {
        // Find where `function=` or `function>` appears and insert `<` before it
        if let Some(pos) = content.find("function=").or_else(|| content.find("function>")) {
            let mut s = content.to_string();
            if pos == 0 || content.as_bytes().get(pos.wrapping_sub(1)) != Some(&b'<') {
                s.insert(pos, '<');
            }
            std::borrow::Cow::Owned(s)
        } else {
            std::borrow::Cow::Borrowed(content)
        }
    } else {
        std::borrow::Cow::Borrowed(content)
    };

    let blocks: Vec<&str> = if normalized.contains("<tool_call>") {
        normalized
            .split("<tool_call>")
            .skip(1)
            .filter_map(|s| s.split("</tool_call>").next())
            .collect()
    } else {
        vec![&normalized]
    };

    let mut out = vec![];
    for (idx, block) in blocks.iter().enumerate() {
        let Some(name) = xml_function_name(block) else {
            continue;
        };
        let arguments = serde_json::to_string(&xml_parameters(block)).unwrap_or_else(|_| "{}".into());
        out.push(ToolCall {
            id: format!("xml-{name}-{idx}"),
            r#type: "function".into(),
            function: FunctionCall { name, arguments },
        });
    }
    out
}

fn xml_function_name(block: &str) -> Option<String> {
    let start = block.find("<function=")? + "<function=".len();
    let rest = &block[start..];
    let end = rest.find('>')?;
    let name = rest[..end].trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

fn xml_parameters(block: &str) -> serde_json::Map<String, serde_json::Value> {
    let mut map = serde_json::Map::new();
    let mut cursor = block;
    // Support <parameter , <parameter= , bare parameter=
    while let Some(start) = cursor.find("<parameter").or_else(|| cursor.find("parameter=")) {
        let after_start = &cursor[start..];
        let marker_len = if after_start.starts_with("<parameter=") {
            "<parameter=".len()
        } else if after_start.starts_with("parameter=") {
            "parameter=".len()
        } else if after_start.starts_with("<parameter") {
            // find = or >
            if let Some(eq) = after_start.find('=') {
                eq + 1
            } else if let Some(gt) = after_start.find('>') {
                gt + 1  // key might be missing, skip
            } else {
                cursor = &cursor[start + 1..];
                continue;
            }
        } else {
            "parameter=".len()
        };
        let after_key = &cursor[start + marker_len..];
        let Some(key_end) = after_key.find('>') else {
            break;
        };
        let key = after_key[..key_end].trim().to_string();
        let value_block = &after_key[key_end + 1..];
        let Some(value_end) = value_block.find("</parameter>").or_else(|| value_block.find("<parameter")).or_else(|| value_block.find("parameter=")) else {
            // take to end if no close
            let value = value_block.trim();
            if !value.is_empty() {
                map.insert(key, serde_json::Value::String(value.to_string()));
            }
            break;
        };
        let value = value_block[..value_end].trim();
        if !value.is_empty() {
            map.insert(key, serde_json::Value::String(value.to_string()));
        }
        cursor = &value_block[value_end..];
    }
    map
}

pub(crate) fn contains_tool_xml_syntax(text: &str) -> bool {
    let t = text.to_lowercase();
    if t.contains("<tool_call") || t.contains("</tool_call") ||
       t.contains("function=") || t.contains("<function=") || t.contains("function>") ||
       t.contains("<parameter") || t.contains("parameter=") || t.contains("parameter ") ||
       t.contains("</parameter") || t.contains("</function") ||
       t.contains("path>") ||
       // partial outputs like "read><parameter" or "read>"
       t.contains("read>") || t.contains("write>") || t.contains("patch>") ||
       t.contains("exec>") || t.contains("grep>") || t.contains("list>")
    {
        return true;
    }
    // Also flag very short texts that are exactly / primarily a bare tool verb.
    // Safe because this only fires for short fragments; long narrative containing
    // the word "read" etc. will have been caught (or not) by the above and will
    // have other sentence structure.
    let trimmed = text.trim();
    if trimmed.len() <= 12 {
        let l = trimmed.to_lowercase();
        if matches!(l.as_str(), "function" | "read" | "write" | "patch" | "exec" | "grep" | "list" | "parameter")
            || l == "function" || l.starts_with("function") || l.starts_with("read>") || l.starts_with("write>")
        {
            return true;
        }
    }
    false
}

/// Remove XML-style tool call blocks (and any immediately preceding stray "}"
/// or punctuation that was part of a mixed-generation prefix) from assistant
/// content.
///
/// This is used for the Qwen/llama.cpp-style fallback where the model emits
/// `function=...` or `<tool_call>...</tool_call>` (or `}<function...` or stray
/// closing tags like `}</parameter></function></tool_call>`, or bare fragments
/// like `parameter=path>...` or `=path>...`) inside the normal content stream.
///
/// We keep any real narrative the model produced before the tool syntax,
/// but we do *not* want raw/partial tool XML or a dangling "}" stored in
/// conversation history or treated as the model's "final text" / Agent output.
pub fn strip_xml_tool_call_blocks(content: &str) -> String {
    if content.trim().is_empty() {
        return String::new();
    }

    // Use regex for more robust detection of any tool call syntax fragment, including partials and variants like "read><parameter "
    // This is more reliable than a static list of contains for catching model-specific or truncated emissions.
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?i)(<tool_call|</tool_call|function=|<function=|function>|<parameter|parameter=|parameter |</parameter|</function|path>|read>|write>|patch>|exec>|grep>|list>)").unwrap()
    });

    let mut s = if let Some(mat) = RE.find(content) {
        content[0..mat.start()].to_string()
    } else {
        content.to_string()
    };

    // Aggressively remove any remaining fragments (regex replace for variants)
    static REMOVE_RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?i)</?tool_call>|</?function[^>]*>|</?parameter[^>]*>|\bfunction=[^>\s]*>|\bpath>[^\s<]*|read>|write>|patch>|exec>|grep>|list>").unwrap()
    });
    s = REMOVE_RE.replace_all(&s, "").to_string();

    let mut out = s.trim().to_string();

    // Drop pure junk / dangling closer that was right before the XML (the exact case reported)
    let cleaned = out.trim_end_matches(|c: char| " \t\n\r{}[],.;:()".contains(c)).trim().to_string();
    if out == "}" || out == "}," || cleaned.is_empty() || (out.len() <= 5 && !out.chars().any(|c| c.is_alphanumeric())) {
        return String::new();
    }

    // Treat very short responses that are exactly a bare tool verb (or common prefix of one)
    // as XML leakage even if the completing "=..." or ">" never arrived in this content stream.
    // These can leak when the first token(s) are forwarded before full marker is visible in full_text.
    let lower = out.to_lowercase();
    if matches!(lower.as_str(), "function" | "read" | "write" | "patch" | "exec" | "grep" | "list" | "parameter")
        || (out.len() <= 12 && (lower == "function" || lower.starts_with("function") || lower.starts_with("read") || lower.starts_with("write") || lower.starts_with("patch")))
    {
        return String::new();
    }

    // If the remaining ends with a stray closer, trim it.
    if out.ends_with('}') || out.ends_with("},") {
        let core = out.trim_end_matches(|c: char| " \t\n\r{}[],.;:()".contains(c)).trim().to_string();
        if core.is_empty() || core.len() + 3 >= out.len() {
            out = core;
        }
    }

    out.trim().to_string()
}

/// Detect whether a base URL points to OpenRouter.
pub fn is_openrouter(base_url: &str) -> bool {
    base_url.contains("openrouter.ai")
}

/// Whether this endpoint has a metered (paid) balance we can query.
pub fn is_metered_endpoint(base_url: &str) -> bool {
    is_openrouter(base_url)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OpenRouterBalance(f64);

/// Status-bar label for credits: `$∞` for local/unmetered, `$12.34` for OpenRouter.
pub async fn balance_label_for(base_url: &str, api_key: Option<&str>) -> String {
    if !is_metered_endpoint(base_url) {
        return "$∞".to_string();
    }
    let Some(key) = api_key.filter(|k| !k.is_empty()) else {
        return "$—".to_string();
    };
    match fetch_openrouter_balance(key).await {
        Some(OpenRouterBalance(d)) => format!("${:.2}", d.max(0.0)),
        None => "$—".to_string(),
    }
}

/// Query remaining OpenRouter credits for the authenticated account/key.
pub async fn fetch_openrouter_balance(api_key: &str) -> Option<OpenRouterBalance> {
    let client = Client::builder()
        .timeout(Duration::from_secs(8))
        .build()
        .ok()?;

    // Account wallet balance (total purchased − used). Works with inference API keys.
    if let Some(balance) = fetch_openrouter_credits(&client, api_key).await {
        return Some(balance);
    }

    // Fallback: per-key spending cap when configured.
    fetch_openrouter_key_limit(&client, api_key).await
}

async fn openrouter_get(
    client: &Client,
    path: &str,
    api_key: &str,
) -> Option<serde_json::Value> {
    let resp = client
        .get(format!("https://openrouter.ai/api/v1{path}"))
        .bearer_auth(api_key)
        .header("HTTP-Referer", "https://github.com/ixmon/raven-harness")
        .header("X-Title", "Raven Hotel TUI")
        .send()
        .await
        .ok()?;

    if !resp.status().is_success() {
        return None;
    }

    resp.json().await.ok()
}

async fn fetch_openrouter_credits(client: &Client, api_key: &str) -> Option<OpenRouterBalance> {
    let body = openrouter_get(client, "/credits", api_key).await?;
    parse_openrouter_credits_balance(&body)
}

async fn fetch_openrouter_key_limit(client: &Client, api_key: &str) -> Option<OpenRouterBalance> {
    let body = openrouter_get(client, "/key", api_key).await?;
    parse_openrouter_key_limit(&body)
}

/// `GET /credits` → `total_credits - total_usage` (account wallet).
fn parse_openrouter_credits_balance(body: &serde_json::Value) -> Option<OpenRouterBalance> {
    let data = body.get("data")?;
    let total = data.get("total_credits")?.as_f64()?;
    let used = data.get("total_usage")?.as_f64()?;
    Some(OpenRouterBalance((total - used).max(0.0)))
}

/// `GET /key` → `limit_remaining` when a per-key cap is configured.
/// `null` means no per-key cap (not unlimited account balance).
fn parse_openrouter_key_limit(body: &serde_json::Value) -> Option<OpenRouterBalance> {
    let remaining = body.get("data")?.get("limit_remaining")?;
    if remaining.is_null() {
        return None;
    }
    remaining.as_f64().map(OpenRouterBalance)
}

/// Accumulate streaming tool-call fragments keyed by index.
pub(crate) fn accumulate_tool_call_deltas(
    tool_accum: &mut HashMap<usize, ToolCall>,
    tool_deltas: &[serde_json::Value],
) {
    for td in tool_deltas {
        let idx = td
            .get("index")
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


#[cfg(test)]
fn parse_sse_data_payload(data_str: &str) -> SseParseResult {
    if data_str == "[DONE]" {
        return SseParseResult::Done;
    }

    let Ok(json) = serde_json::from_str::<serde_json::Value>(data_str) else {
        return SseParseResult::Ignore;
    };

    if let Some(usage_val) = json.get("usage") {
        if let Ok(u) = serde_json::from_value::<Usage>(usage_val.clone()) {
            return SseParseResult::Usage(u);
        }
    }

    let Some(choices) = json.get("choices").and_then(|c| c.as_array()) else {
        return SseParseResult::Ignore;
    };
    let Some(choice) = choices.first() else {
        return SseParseResult::Ignore;
    };

    let delta = choice.get("delta").cloned().unwrap_or_else(|| json!({}));

    if let Some(thinking) = delta
        .get("reasoning_content")
        .and_then(|v| v.as_str())
        .or_else(|| delta.get("reasoning").and_then(|v| v.as_str()))
    {
        if !thinking.is_empty() {
            return SseParseResult::Thinking(thinking.to_string());
        }
    }

    if let Some(token) = delta.get("content").and_then(|v| v.as_str()) {
        if !token.is_empty() {
            return SseParseResult::Token(token.to_string());
        }
    }

    if let Some(tool_deltas) = delta.get("tool_calls").and_then(|t| t.as_array()) {
        return SseParseResult::ToolDeltas(tool_deltas.clone());
    }

    SseParseResult::Ignore
}

#[derive(Debug, Clone, PartialEq)]
#[cfg(test)]
enum SseParseResult {
    Ignore,
    Done,
    Token(String),
    Thinking(String),
    ToolDeltas(Vec<serde_json::Value>),
    Usage(Usage),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accumulate_tool_call_deltas_across_chunks() {
        let mut acc = HashMap::new();

        let chunk1 = serde_json::json!([{
            "index": 0,
            "id": "call_abc",
            "function": { "name": "read", "arguments": "{\"path\":" }
        }]);
        accumulate_tool_call_deltas(&mut acc, chunk1.as_array().unwrap());

        let chunk2 = serde_json::json!([{
            "index": 0,
            "function": { "arguments": "\"main.rs\"}" }
        }]);
        accumulate_tool_call_deltas(&mut acc, chunk2.as_array().unwrap());

        let chunk3 = serde_json::json!([{
            "index": 1,
            "id": "call_def",
            "function": { "name": "grep", "arguments": "{}" }
        }]);
        accumulate_tool_call_deltas(&mut acc, chunk3.as_array().unwrap());

        assert_eq!(acc.len(), 2);
        let tc0 = acc.get(&0).unwrap();
        assert_eq!(tc0.id, "call_abc");
        assert_eq!(tc0.function.name, "read");
        assert_eq!(tc0.function.arguments, "{\"path\":\"main.rs\"}");

        let tc1 = acc.get(&1).unwrap();
        assert_eq!(tc1.id, "call_def");
        assert_eq!(tc1.function.name, "grep");
    }

    #[test]
    fn parse_sse_token_chunk() {
        let payload = r#"{"choices":[{"delta":{"content":"hello"}}]}"#;
        assert_eq!(
            parse_sse_data_payload(payload),
            SseParseResult::Token("hello".into())
        );
    }

    #[test]
    fn parse_sse_tool_delta_chunk() {
        let payload = r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"exec"}}]}}]}"#;
        match parse_sse_data_payload(payload) {
            SseParseResult::ToolDeltas(deltas) => {
                assert_eq!(deltas.len(), 1);
                assert_eq!(
                    deltas[0].get("function").unwrap().get("name").unwrap(),
                    "exec"
                );
            }
            other => panic!("expected ToolDeltas, got {:?}", other),
        }
    }

    #[test]
    fn parse_sse_done_sentinel() {
        assert_eq!(parse_sse_data_payload("[DONE]"), SseParseResult::Done);
    }

    #[test]
    fn parse_xml_tool_calls_from_qwen_style_content() {
        let content = r#"<tool_call>
<function=list>
<parameter=path>
.
</parameter>
</function>
</tool_call>
<tool_call>
<function=read_summary>
<parameter=path>
README.md
</parameter>
</function>
</tool_call>"#;
        let calls = parse_xml_tool_calls_from_content(content);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].function.name, "list");
        assert_eq!(calls[0].function.arguments, r#"{"path":"."}"#);
        assert_eq!(calls[1].function.name, "read_summary");
        assert_eq!(calls[1].function.arguments, r#"{"path":"README.md"}"#);
    }

    #[test]
    fn parse_xml_tool_calls_truncated_streaming_format() {
        // llama.cpp streaming sometimes strips the leading `<tool_call>\n<`
        // so the content starts with bare `function=name>`.
        let content = "function=list>\n<parameter=path>\n.\n</parameter>\n</function>\n</tool_call>";
        let calls = parse_xml_tool_calls_from_content(content);
        assert_eq!(calls.len(), 1, "should parse truncated XML: {:?}", calls);
        assert_eq!(calls[0].function.name, "list");
        assert_eq!(calls[0].function.arguments, r#"{"path":"."}"#);
    }

    #[test]
    fn strip_xml_tool_call_blocks_removes_stray_closer_plus_closing_tags() {
        // Exact case reported: model emitted "}" + tail of XML tool call syntax
        // (no opening "function=" seen by this fragment). Must become empty so we
        // never push or display it as Agent text / assistant content.
        let bad = "}\n</parameter>\n</function>\n</tool_call>";
        assert_eq!(strip_xml_tool_call_blocks(bad), "");

        // Also handles mixed with opening
        let mixed = "I will call the tool now}\n<tool_call>\n<function=read>\n<parameter=path>src/foo.py</parameter>\n</function>\n</tool_call>";
        let cleaned = strip_xml_tool_call_blocks(mixed);
        assert!(!cleaned.contains("function="), "should strip XML: {}", cleaned);
        assert!(!cleaned.contains("</tool_call>"), "should strip XML: {}", cleaned);
        assert!(cleaned.contains("I will call"), "should keep prefix narrative");

        // Pure opening form also stripped to empty when no narrative
        let pure = "function=patch>\n<parameter=path>bar.rs</parameter>\n</function>\n</tool_call>";
        assert_eq!(strip_xml_tool_call_blocks(pure), "");

        // Fragment like the reported case (model emitted tail of XML syntax)
        let frag = "=path>  src/marshmallow/schema.py";
        assert_eq!(strip_xml_tool_call_blocks(frag), "");

        let frag2 = "parameter=path> src/marshmallow/schema.py\n</parameter>";
        assert_eq!(strip_xml_tool_call_blocks(frag2), "");
    }

    #[test]
    fn assistant_text_prefers_content_over_reasoning() {
        let msg = json!({
            "content": "hello",
            "reasoning_content": "thinking"
        });
        assert_eq!(assistant_text_from_message(&msg), "hello");
    }

    #[test]
    fn assistant_text_falls_back_to_reasoning() {
        let msg = json!({
            "content": "",
            "reasoning_content": "SMOKE_OK\n"
        });
        assert_eq!(assistant_text_from_message(&msg), "SMOKE_OK");
    }

    #[test]
    fn parse_sse_thinking_chunk() {
        let payload = r#"{"choices":[{"delta":{"reasoning_content":"hmm"}}]}"#;
        assert_eq!(
            parse_sse_data_payload(payload),
            SseParseResult::Thinking("hmm".into())
        );
    }

    #[test]
    fn parses_openrouter_credits_balance() {
        let body = serde_json::json!({
            "data": { "total_credits": 10.0, "total_usage": 2.5 }
        });
        assert_eq!(
            parse_openrouter_credits_balance(&body),
            Some(OpenRouterBalance(7.5))
        );
    }

    #[test]
    fn parse_openrouter_credits_zero_balance() {
        let body = serde_json::json!({
            "data": { "total_credits": 0.0, "total_usage": 0.0 }
        });
        assert_eq!(
            parse_openrouter_credits_balance(&body),
            Some(OpenRouterBalance(0.0))
        );
    }

    #[test]
    fn parse_openrouter_key_limit_remaining() {
        let body = serde_json::json!({
            "data": { "limit_remaining": 12.5, "usage": 3.25 }
        });
        assert_eq!(
            parse_openrouter_key_limit(&body),
            Some(OpenRouterBalance(12.5))
        );
    }

    #[test]
    fn parse_openrouter_key_limit_null_is_not_unlimited() {
        let body = serde_json::json!({
            "data": { "limit_remaining": null }
        });
        assert_eq!(parse_openrouter_key_limit(&body), None);
    }

}
