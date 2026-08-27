//! LLM Provider trait definition.

use anyhow::Result;
use async_trait::async_trait;
use futures::Stream;
use serde::{Deserialize, Serialize};
use std::pin::Pin;

/// A single message in a chat conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Reasoning content from thinking models (e.g., kimi-k2.6).
    /// Must be preserved and sent back in subsequent requests.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
}

/// A tool call from the LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub r#type: String,
    pub function: FunctionCall,
}

/// A function call within a tool call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

/// A tool definition for the LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub r#type: String,
    pub function: FunctionDefinition,
}

/// A function definition within a tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// Usage statistics from an LLM response.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Usage {
    pub prompt_tokens: i32,
    pub completion_tokens: i32,
    pub total_tokens: i32,
}

/// A streaming chunk from the LLM.
#[derive(Debug, Clone)]
pub enum StreamChunk {
    /// A text content delta.
    Delta(String),
    /// A tool call delta (call_id, function_name, arguments_delta).
    ToolCallDelta(String, String, String),
    /// Reasoning content delta (from thinking models like kimi-k2.6).
    ReasoningDelta(String),
    /// Stream completed with usage stats.
    Done(Usage),
}

/// A complete (non-streaming) chat response.
#[derive(Debug, Clone)]
pub struct ChatResponse {
    pub message: ChatMessage,
    pub usage: Usage,
}

/// Type alias for a boxed stream of chunks.
pub type ChatStream = Pin<Box<dyn Stream<Item = Result<StreamChunk>> + Send>>;

/// Trait that all LLM providers must implement.
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Provider name (e.g., "openai", "anthropic").
    fn name(&self) -> &str;

    /// Send a chat request and get a complete response.
    async fn chat(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
    ) -> Result<ChatResponse>;

    /// Send a chat request and get a streaming response.
    async fn chat_stream(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
    ) -> Result<ChatStream>;

    /// Get the model's context length.
    fn context_length(&self) -> usize;
}

/// Mock LLM provider for testing.
#[cfg(any(test, feature = "test-utils"))]
pub struct MockLlmProvider {
    context_len: usize,
}

#[cfg(any(test, feature = "test-utils"))]
impl MockLlmProvider {
    pub fn new() -> Self {
        Self { context_len: 128000 }
    }

    pub fn with_context_length(context_len: usize) -> Self {
        Self { context_len }
    }
}

#[cfg(any(test, feature = "test-utils"))]
impl Default for MockLlmProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(any(test, feature = "test-utils"))]
#[async_trait]
impl LlmProvider for MockLlmProvider {
    fn name(&self) -> &str {
        "mock"
    }

    async fn chat(
        &self,
        _messages: &[ChatMessage],
        _tools: &[ToolDefinition],
    ) -> Result<ChatResponse> {
        Ok(ChatResponse {
            message: ChatMessage {
                role: "assistant".to_string(),
                content: "This is a mock response for testing.".to_string(),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            },
            usage: Usage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
            },
        })
    }

    async fn chat_stream(
        &self,
        _messages: &[ChatMessage],
        _tools: &[ToolDefinition],
    ) -> Result<ChatStream> {
        use futures::stream::{self, StreamExt};
        
        let chunks = vec![
            Ok(StreamChunk::Delta("Mock ".to_string())),
            Ok(StreamChunk::Delta("response ".to_string())),
            Ok(StreamChunk::Delta("for testing.".to_string())),
            Ok(StreamChunk::Done(Usage {
                prompt_tokens: 10,
                completion_tokens: 3,
                total_tokens: 13,
            })),
        ];
        
        Ok(Box::pin(stream::iter(chunks)))
    }

    fn context_length(&self) -> usize {
        self.context_len
    }
}
