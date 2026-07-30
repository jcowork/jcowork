//! OpenAI-compatible LLM provider with SSE streaming.

use anyhow::{Context, Result};
use async_trait::async_trait;
use futures::{Stream, StreamExt};
use reqwest::Client;
use serde::Deserialize;

use crate::provider::{
    ChatMessage, ChatResponse, ChatStream, FunctionCall, LlmProvider, StreamChunk, ToolCall,
    ToolDefinition, Usage,
};

/// Configuration for the OpenAI-compatible provider.
#[derive(Debug, Clone)]
pub struct OpenAiConfig {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub context_length: usize,
    /// Provider name override (defaults to "openai").
    pub provider_name: String,
}

impl Default for OpenAiConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            base_url: "https://api.openai.com/v1".to_string(),
            model: "gpt-4o".to_string(),
            context_length: 128_000,
            provider_name: "openai".to_string(),
        }
    }
}

/// OpenAI-compatible LLM provider.
///
/// Works with OpenAI, OpenRouter, any OpenAI-compatible API.
pub struct OpenAiProvider {
    client: Client,
    config: OpenAiConfig,
}

impl OpenAiProvider {
    pub fn new(config: OpenAiConfig) -> Self {
        let client = Client::new();
        Self { client, config }
    }

    /// Build the request body.
    fn build_request_body(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        stream: bool,
    ) -> serde_json::Value {
        let mut body = serde_json::json!({
            "model": self.config.model,
            "messages": messages,
            "stream": stream,
        });

        if !tools.is_empty() {
            body["tools"] = serde_json::json!(tools);
            body["tool_choice"] = serde_json::json!("auto");
        }

        body
    }
}

#[derive(Debug, Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<Choice>,
    usage: Option<UsageRaw>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: MessageRaw,
}

#[derive(Debug, Deserialize)]
struct MessageRaw {
    role: String,
    content: Option<String>,
    tool_calls: Option<Vec<ToolCallRaw>>,
    /// Reasoning content from thinking models.
    reasoning_content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ToolCallRaw {
    id: String,
    r#type: String,
    function: FunctionCallRaw,
}

#[derive(Debug, Deserialize)]
struct FunctionCallRaw {
    name: String,
    arguments: String,
}

#[derive(Debug, Deserialize)]
struct UsageRaw {
    prompt_tokens: i32,
    completion_tokens: i32,
    total_tokens: i32,
}

#[derive(Debug, Deserialize)]
struct StreamResponse {
    choices: Vec<StreamChoice>,
    usage: Option<UsageRaw>,
}

#[derive(Debug, Deserialize)]
struct StreamChoice {
    delta: StreamDelta,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct StreamDelta {
    #[allow(dead_code)]
    role: Option<String>,
    content: Option<String>,
    tool_calls: Option<Vec<StreamToolCall>>,
    /// Reasoning content from thinking models (kimi-k2.6).
    reasoning_content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct StreamToolCall {
    index: Option<i32>,
    id: Option<String>,
    #[allow(dead_code)]
    r#type: Option<String>,
    function: Option<StreamFunction>,
}

#[derive(Debug, Deserialize)]
struct StreamFunction {
    name: Option<String>,
    arguments: Option<String>,
}

#[async_trait]
impl LlmProvider for OpenAiProvider {
    fn name(&self) -> &str {
        &self.config.provider_name
    }

    async fn chat(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
    ) -> Result<ChatResponse> {
        let body = self.build_request_body(messages, tools, false);
        let url = format!("{}/chat/completions", self.config.base_url);

        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.config.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .context("Failed to send chat request")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await?;
            anyhow::bail!("LLM API error {}: {}", status, text);
        }

        let completion: ChatCompletionResponse =
            resp.json().await.context("Failed to parse chat response")?;

        let choice = completion
            .choices
            .into_iter()
            .next()
            .context("No choices in response")?;

        let message = ChatMessage {
            role: choice.message.role,
            content: choice.message.content.unwrap_or_default(),
            tool_calls: choice.message.tool_calls.map(|tcs| {
                tcs.into_iter()
                    .map(|tc| ToolCall {
                        id: tc.id,
                        r#type: tc.r#type,
                        function: FunctionCall {
                            name: tc.function.name,
                            arguments: tc.function.arguments,
                        },
                    })
                    .collect()
            }),
            tool_call_id: None,
            reasoning_content: choice.message.reasoning_content,
        };

        let usage = completion
            .usage
            .map(|u| Usage {
                prompt_tokens: u.prompt_tokens,
                completion_tokens: u.completion_tokens,
                total_tokens: u.total_tokens,
            })
            .unwrap_or_default();

        Ok(ChatResponse { message, usage })
    }

    async fn chat_stream(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
    ) -> Result<ChatStream> {
        let body = self.build_request_body(messages, tools, true);
        let url = format!("{}/chat/completions", self.config.base_url);

        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.config.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .context("Failed to send streaming chat request")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await?;
            anyhow::bail!("LLM API error {}: {}", status, text);
        }

        let stream = parse_sse_stream(resp.bytes_stream());
        Ok(Box::pin(stream))
    }

    fn context_length(&self) -> usize {
        self.config.context_length
    }
}

/// Parse an SSE byte stream into StreamChunks.
fn parse_sse_stream(
    byte_stream: impl Stream<Item = Result<bytes::Bytes, reqwest::Error>>,
) -> impl Stream<Item = Result<StreamChunk>> {

    #[derive(Default)]
    struct ToolCallBuffer {
        id: String,
        name: String,
        arguments: String,
        emitted_len: usize,
    }

    let mut buffer = String::new();
    let mut tool_call_buffers: std::collections::HashMap<usize, ToolCallBuffer> =
        std::collections::HashMap::new();

    byte_stream.flat_map(move |chunk_result| {
        let mut outputs = Vec::new();

        match chunk_result {
            Ok(bytes) => {
                buffer.push_str(&String::from_utf8_lossy(&bytes));

                while let Some(pos) = buffer.find("\n\n") {
                    let event_str = buffer[..pos].to_string();
                    buffer = buffer[pos + 2..].to_string();

                    for line in event_str.lines() {
                        // Support both "data: "(standard) and "data: " (some OpenAI-compatible APIs like GLM)
                        let data_opt = line.strip_prefix("data: ").or_else(|| line.strip_prefix("data:"));
                        if let Some(data) = data_opt {
                            if data == "[DONE]" {
                                outputs.push(Ok(StreamChunk::Done(Usage::default())));
                                continue;
                            }

                            match serde_json::from_str::<StreamResponse>(data) {
                                Ok(resp) => {
                                    for choice in resp.choices {
                                        // Text content
                                        if let Some(content) = &choice.delta.content {
                                            outputs.push(Ok(StreamChunk::Delta(content.clone())));
                                        }

                                        // Reasoning content (thinking models)
                                        if let Some(reasoning) = &choice.delta.reasoning_content {
                                            outputs.push(Ok(StreamChunk::ReasoningDelta(reasoning.clone())));
                                        }

                                        // Tool calls
                                        if let Some(tool_calls) = choice.delta.tool_calls {
                                            for tc in tool_calls {
                                                let idx = tc.index.unwrap_or(0) as usize;
                                                let entry = tool_call_buffers.entry(idx).or_default();

                                                if let Some(id) = tc.id {
                                                    if !id.is_empty() {
                                                        entry.id = id;
                                                    }
                                                }
                                                if let Some(func) = tc.function {
                                                    if let Some(name) = func.name {
                                                        if !name.is_empty() {
                                                            entry.name.push_str(&name);
                                                        }
                                                    }
                                                    if let Some(args) = func.arguments {
                                                        entry.arguments.push_str(&args);
                                                    }
                                                }

                                                if !entry.id.is_empty() && !entry.name.is_empty() {
                                                    let new_args = &entry.arguments[entry.emitted_len..];
                                                    if !new_args.is_empty() {
                                                        entry.emitted_len = entry.arguments.len();
                                                        outputs.push(Ok(StreamChunk::ToolCallDelta(
                                                            entry.id.clone(),
                                                            entry.name.clone(),
                                                            new_args.to_string(),
                                                        )));
                                                    }
                                                }
                                            }
                                        }

                                        // Finish
                                        if choice.finish_reason.as_deref() == Some("stop")
                                            || choice.finish_reason.as_deref() == Some("tool_calls")
                                        {
                                            if let Some(usage) = &resp.usage {
                                                outputs.push(Ok(StreamChunk::Done(Usage {
                                                    prompt_tokens: usage.prompt_tokens,
                                                    completion_tokens: usage.completion_tokens,
                                                    total_tokens: usage.total_tokens,
                                                })));
                                            } else {
                                                outputs.push(Ok(StreamChunk::Done(Usage::default())));
                                            }
                                        }
                                    }
                                }
                                Err(e) => {
                                    // Check if this is an API error response (e.g. {"error": {...}})
                                    if let Ok(val) = serde_json::from_str::<serde_json::Value>(data) {
                                        if let Some(err) = val.get("error") {
                                            let msg = err.get("message").and_then(|m| m.as_str()).unwrap_or("Unknown API error");
                                            outputs.push(Err(anyhow::anyhow!("API error: {}", msg)));
                                            continue;
                                        }
                                    }
                                    tracing::warn!("Failed to parse SSE data: {} - {}", data, e);
                                }
                            }
                        }
                    }
                }
            }
            Err(e) => {
                outputs.push(Err(anyhow::anyhow!("Stream error: {}", e)));
            }
        }

        futures::stream::iter(outputs)
    })
}
