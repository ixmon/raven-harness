//! Chat completion backends — HTTP (OpenAI-compatible) vs scripted eval mocks.

use std::sync::Mutex;

use anyhow::Result;
use tokio::sync::mpsc;

use crate::config::Config;
use crate::llm::{ChatRequest, ChatResponse, LlmClient, StreamChunk, ToolCall, Usage};

/// LLM transport used by the agent loop.
pub enum ChatBackend {
    Http(Box<LlmClient>),
    Mock(MockChatBackend),
}

impl ChatBackend {
    pub fn http(config: Config) -> Self {
        Self::Http(Box::new(LlmClient::new(config)))
    }

    pub async fn chat(&self, req: ChatRequest) -> Result<ChatResponse> {
        match self {
            Self::Http(c) => c.chat(req).await,
            Self::Mock(m) => m.chat(req).await,
        }
    }

    pub async fn chat_stream(&self, req: ChatRequest) -> Result<mpsc::Receiver<StreamChunk>> {
        match self {
            Self::Http(c) => c.chat_stream(req).await,
            Self::Mock(m) => m.chat_stream(req).await,
        }
    }

    pub fn reset_http(&mut self, config: Config) {
        *self = Self::Http(Box::new(LlmClient::new(config)));
    }
}

/// Deterministic LLM responses for harness tests (`RAVEN_EVAL_MOCK_LLM=1`).
#[derive(Debug)]
pub struct MockChatBackend {
    turns: Mutex<Vec<ChatResponse>>,
    empty_fallback: ChatResponse,
}

impl MockChatBackend {
    pub fn new(scripted: Vec<ChatResponse>) -> Self {
        Self {
            turns: Mutex::new(scripted),
            empty_fallback: ChatResponse {
                content: "(mock llm: no more scripted turns)".into(),
                tool_calls: vec![],
                finish_reason: Some("stop".into()),
                usage: Some(Usage {
                    prompt_tokens: Some(0),
                    completion_tokens: Some(0),
                    total_tokens: Some(0),
                }),
            },
        }
    }

    pub async fn chat(&self, _req: ChatRequest) -> Result<ChatResponse> {
        let mut turns = self.turns.lock().expect("mock llm lock");
        if turns.is_empty() {
            return Ok(self.empty_fallback.clone());
        }
        Ok(turns.remove(0))
    }

    pub async fn chat_stream(&self, req: ChatRequest) -> Result<mpsc::Receiver<StreamChunk>> {
        let (tx, rx) = mpsc::channel(8);
        let resp = self.chat(req).await?;
        let _ = tx
            .send(StreamChunk::Done {
                content: resp.content,
                tool_calls: resp.tool_calls,
                usage: resp.usage,
            })
            .await;
        Ok(rx)
    }
}

/// Build a `ToolCall` with a stable synthetic id for eval scripts.
pub fn mock_tool_call(name: &str, arguments: &str) -> ToolCall {
    ToolCall {
        id: format!("mock-{name}"),
        r#type: "function".into(),
        function: crate::llm::FunctionCall {
            name: name.into(),
            arguments: arguments.into(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mock_backend_pops_scripted_turns() {
        let backend = ChatBackend::Mock(MockChatBackend::new(vec![
            ChatResponse {
                content: String::new(),
                tool_calls: vec![mock_tool_call("list", r#"{"path":"."}"#)],
                finish_reason: None,
                usage: None,
            },
            ChatResponse {
                content: "done".into(),
                tool_calls: vec![],
                finish_reason: Some("stop".into()),
                usage: None,
            },
        ]));

        let req = ChatRequest {
            messages: vec![],
            tools: None,
            temperature: 0.0,
            max_tokens: 100,
            stream: false,
        };

        let r1 = backend.chat(req.clone()).await.unwrap();
        assert_eq!(r1.tool_calls.len(), 1);
        let r2 = backend.chat(req).await.unwrap();
        assert_eq!(r2.content, "done");
    }
}