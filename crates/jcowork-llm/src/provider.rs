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
