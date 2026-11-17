//! OpenAI-compatible LLM provider with SSE streaming.

use anyhow::{Context, Result};
use async_trait::async_trait;
use futures::{Stream, StreamExt};
use reqwest::Client;
use serde::Deserialize;
use std::sync::Arc;

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
    /// Ask streaming APIs to report token usage via
    /// `stream_options.include_usage`. Disable it for gateways that reject
    /// unknown request fields — such providers then yield zero-token usage.
    pub include_usage: bool,
}

impl Default for OpenAiConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            base_url: "https://api.openai.com/v1".to_string(),
            model: "gpt-4o".to_string(),
            context_length: 128_000,
            provider_name: "openai".to_string(),
            include_usage: true,
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
        // Serialize messages, expanding image attachments into OpenAI
        // multimodal content parts (text + image_url) when present.
        let messages_json: Vec<serde_json::Value> = messages
            .iter()
            .map(|m| {
                let mut v = serde_json::to_value(m).unwrap_or_default();
                if let Some(obj) = v.as_object_mut() {
                    obj.remove("images");
                }
                if let Some(images) = m.images.as_ref().filter(|imgs| !imgs.is_empty()) {
                    let mut parts = vec![serde_json::json!({
                        "type": "text",
                        "text": m.content,
                    })];
                    for url in images {
                        parts.push(serde_json::json!({
                            "type": "image_url",
                            "image_url": { "url": url },
                        }));
                    }
                    v["content"] = serde_json::json!(parts);
                }
                v
            })
            .collect();

        let mut body = serde_json::json!({
            "model": self.config.model,
            "messages": messages_json,
            "stream": stream,
        });

        // Streaming APIs report token usage only when asked: the last chunk
        // then carries `usage` alongside an empty `choices` array. Without
        // this switch every streamed request reports zero tokens.
        if stream && self.include_usage() {
            body["stream_options"] = serde_json::json!({ "include_usage": true });
        }

        if !tools.is_empty() {
            body["tools"] = serde_json::json!(tools);
            body["tool_choice"] = serde_json::json!("auto");
        }

        body
    }

    /// Whether streaming usage reporting is enabled for this provider.
    ///
    /// `JCWORK_STREAM_USAGE=0` turns it off globally for gateways that reject
    /// `stream_options`.
    fn include_usage(&self) -> bool {
        if !self.config.include_usage {
            return false;
        }
        match std::env::var("JCWORK_STREAM_USAGE") {
            Ok(v) => !matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "off" | "no"
            ),
            Err(_) => true,
        }
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
            images: None,
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

    fn with_model(&self, model: &str) -> Arc<dyn LlmProvider> {
        let mut config = self.config.clone();
        config.model = model.to_string();
        Arc::new(Self::new(config))
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
    // Last usage seen in the stream, and whether it was already emitted. A
    // `Done` is pushed at most once per stream: with `include_usage` the real
    // usage chases the finish chunk, and a zero fallback must not overwrite it.
    let mut pending_usage: Option<Usage> = None;
    let mut usage_emitted = false;

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
                            if data.trim() == "[DONE]" {
                                if !usage_emitted {
                                    usage_emitted = true;
                                    outputs.push(Ok(StreamChunk::Done(
                                        pending_usage.take().unwrap_or_default(),
                                    )));
                                }
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
                                    }

                                    // Usage is reported in a trailing chunk whose
                                    // `choices` array is empty, so it must be read
                                    // outside the loop above to avoid being dropped.
                                    if let Some(u) = resp.usage {
                                        let usage = Usage {
                                            prompt_tokens: u.prompt_tokens,
                                            completion_tokens: u.completion_tokens,
                                            total_tokens: u.total_tokens,
                                        };
                                        if usage.has_tokens() && !usage_emitted {
                                            usage_emitted = true;
                                            outputs.push(Ok(StreamChunk::Done(usage.clone())));
                                        }
                                        pending_usage = Some(usage);
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

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    fn provider(include_usage: bool) -> OpenAiProvider {
        OpenAiProvider::new(OpenAiConfig {
            api_key: "test-key".to_string(),
            base_url: "http://localhost:1/v1".to_string(),
            model: "test-model".to_string(),
            context_length: 8_000,
            provider_name: "test".to_string(),
            include_usage,
        })
    }

    /// Feed raw SSE payloads (split into arbitrary network chunks) through
    /// the parser and return the emitted chunks.
    fn parse_events(payloads: &[&str]) -> Vec<StreamChunk> {
        let network_chunks: Vec<_> = payloads
            .iter()
            .map(|p| Ok::<_, reqwest::Error>(bytes::Bytes::from(p.to_string())))
            .collect();
        let stream = parse_sse_stream(futures::stream::iter(network_chunks));
        futures::executor::block_on(stream.collect::<Vec<_>>())
            .into_iter()
            .map(|c| c.expect("stream chunk"))
            .collect()
    }

    fn text_of(chunks: &[StreamChunk]) -> String {
        chunks
            .iter()
            .filter_map(|c| match c {
                StreamChunk::Delta(d) => Some(d.as_str()),
                _ => None,
            })
            .collect()
    }

    fn done_usages(chunks: &[StreamChunk]) -> Vec<&Usage> {
        chunks
            .iter()
            .filter_map(|c| match c {
                StreamChunk::Done(u) => Some(u),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn streaming_request_asks_for_usage() {
        let body = provider(true).build_request_body(&[], &[], true);
        assert_eq!(body["stream"], serde_json::json!(true));
        assert_eq!(
            body["stream_options"]["include_usage"],
            serde_json::json!(true),
            "streaming requests must opt into usage reporting"
        );
    }

    #[test]
    fn non_streaming_request_has_no_stream_options() {
        let body = provider(true).build_request_body(&[], &[], false);
        assert!(body.get("stream_options").is_none());
    }

    #[test]
    fn usage_reporting_can_be_disabled_per_provider() {
        let body = provider(false).build_request_body(&[], &[], true);
        assert!(
            body.get("stream_options").is_none(),
            "providers rejecting stream_options must be able to opt out"
        );
    }

    #[test]
    fn usage_is_read_from_trailing_chunk_without_choices() {
        let chunks = parse_events(&[
            "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"He\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"llo\"},\"finish_reason\":null}]}\n\n",
            // finish chunk: no usage yet
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            // usage-only chunk: empty choices, real usage
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":11,\"completion_tokens\":7,\"total_tokens\":18}}\n\n",
            "data: [DONE]\n\n",
        ]);

        assert_eq!(text_of(&chunks), "Hello");
        let dones = done_usages(&chunks);
        assert_eq!(dones.len(), 1, "exactly one Done per stream");
        assert_eq!(dones[0].prompt_tokens, 11);
        assert_eq!(dones[0].completion_tokens, 7);
        assert_eq!(dones[0].total_tokens, 18);
    }

    #[test]
    fn usage_survives_network_chunk_boundaries() {
        // The usage chunk is sliced across two physical reads.
        let chunks = parse_events(&[
            "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\ndata: {\"choices\":[],\"usage\":{\"prompt_tokens\":3,",
            "\"completion_tokens\":4,\"total_tokens\":7}}\n\ndata: [DONE]\n\n",
        ]);

        assert_eq!(text_of(&chunks), "ok");
        let dones = done_usages(&chunks);
        assert_eq!(dones.len(), 1);
        assert_eq!(dones[0].total_tokens, 7);
    }

    #[test]
    fn stream_without_usage_still_terminates_with_one_done() {
        let chunks = parse_events(&[
            "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
        ]);

        assert_eq!(text_of(&chunks), "hi");
        let dones = done_usages(&chunks);
        assert_eq!(dones.len(), 1);
        assert!(!dones[0].has_tokens(), "missing usage reports zeros");
    }

    #[test]
    fn usage_is_emitted_only_once_when_repeated() {
        let chunks = parse_events(&[
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":6,\"total_tokens\":11}}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":6,\"total_tokens\":11}}\n\n",
            "data: [DONE]\n\n",
        ]);

        assert_eq!(done_usages(&chunks).len(), 1);
        assert_eq!(done_usages(&chunks)[0].total_tokens, 11);
    }
}
