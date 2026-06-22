//! Extract assistant text from OpenAI-compatible chat responses (incl. reasoning models).

use serde_json::Value;

/// Prefer `content`; fall back to `reasoning_content` (Qwen / llama.cpp thinking).
pub fn extract_assistant_text(message: &Value) -> String {
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

pub fn extract_from_chat_response(data: &Value) -> String {
    data.pointer("/choices/0/message")
        .map(extract_assistant_text)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn prefers_content() {
        let msg = json!({"content": "answer", "reasoning_content": "think"});
        assert_eq!(extract_assistant_text(&msg), "answer");
    }

    #[test]
    fn falls_back_to_reasoning_content() {
        let msg = json!({"content": "", "reasoning_content": "SMOKE_OK\n"});
        assert_eq!(extract_assistant_text(&msg), "SMOKE_OK");
    }

    #[test]
    fn extract_from_chat_response_pointer() {
        let data = json!({
            "choices": [{"message": {"content": "", "reasoning_content": "ok"}}]
        });
        assert_eq!(extract_from_chat_response(&data), "ok");
    }
}