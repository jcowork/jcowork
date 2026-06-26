//! Log entry models.

use serde::Serialize;

/// Maximum length for truncated string fields.
const MAX_CONTENT_LEN: usize = 2000;

/// Truncate a string to MAX_CONTENT_LEN, appending "..." if truncated.
fn truncate(s: &str) -> String {
    if s.len() <= MAX_CONTENT_LEN {
        s.to_string()
    } else {
        let mut end = MAX_CONTENT_LEN;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &s[..end])
    }
}

/// A single log entry. Serialized as a JSON Lines record.
#[derive(Debug, Serialize)]
#[serde(tag = "event")]
pub enum LogEntry {
    /// An LLM request/response cycle.
    #[serde(rename = "llm_request")]
    LlmRequest {
        /// ISO 8601 timestamp of when the request started.
        timestamp: String,
        /// User ID that initiated the request.
        user_id: String,
        /// Provider name (e.g., "deepseek", "moonshot").
        provider: String,
        /// Full model string (e.g., "deepseek:deepseek-chat").
        model: String,
        /// Duration from request start to stream completion, in milliseconds.
        duration_ms: u64,
        /// Input summary.
        input: LlmInput,
        /// Output summary.
        output: LlmLogOutput,
    },
    /// A tool call execution.
    #[serde(rename = "tool_call")]
    ToolCall {
        /// ISO 8601 timestamp of when the tool call started.
        timestamp: String,
        /// User ID that initiated the tool call.
        user_id: String,
        /// Tool name (e.g., "memory_search").
        tool: String,
        /// Duration of the tool execution, in milliseconds.
        duration_ms: u64,
        /// Raw input arguments (JSON string).
        input: String,
        /// Tool execution result.
        output: String,
    },
    /// Raw data from external sources (e.g., web search results).
    #[serde(rename = "raw_data")]
    RawData {
        /// ISO 8601 timestamp of when the data was received.
        timestamp: String,
        /// Source of the data (e.g., "web_search").
        source: String,
        /// The raw data (e.g., search results).
        data: serde_json::Value,
    },
}

/// Summary of LLM input (messages sent to the model).
#[derive(Debug, Serialize)]
pub struct LlmInput {
    /// Number of messages in the request.
    pub message_count: usize,
    /// Truncated last user message content.
    pub last_user_message: String,
    /// Full conversation context (all messages with role and truncated content).
    pub messages: Vec<ContextMessage>,
}

/// A single message in the conversation context.
#[derive(Debug, Serialize)]
pub struct ContextMessage {
    /// Message role (system, user, assistant, tool).
    pub role: String,
    /// Truncated message content.
    pub content: String,
}

/// Summary of LLM output (model response).
#[derive(Debug, Serialize)]
pub struct LlmLogOutput {
    /// Truncated assistant text content.
    pub content: String,
    /// Number of tool calls in the response (0 if none).
    pub tool_call_count: usize,
    /// Tool call names (if any).
    pub tool_names: Vec<String>,
    /// Token usage statistics.
    pub usage: UsageSummary,
}

/// Token usage summary.
#[derive(Debug, Serialize)]
pub struct UsageSummary {
    pub prompt_tokens: i32,
    pub completion_tokens: i32,
    pub total_tokens: i32,
}

/// Builder for LlmInput from chat history.
pub fn build_llm_input(messages: &[(impl AsRef<str>, impl AsRef<str>)]) -> LlmInput {
    let last_user_message = messages
        .iter()
        .rev()
        .find(|(role, _)| role.as_ref() == "user")
        .map(|(_, content)| truncate(content.as_ref()))
        .unwrap_or_default();
    let context: Vec<ContextMessage> = messages
        .iter()
        .map(|(role, content)| ContextMessage {
            role: role.as_ref().to_string(),
            content: truncate(content.as_ref()),
        })
        .collect();
    LlmInput {
        message_count: messages.len(),
        last_user_message,
        messages: context,
    }
}

/// Builder for LlmLogOutput from stream results.
pub fn build_llm_output(
    content: &str,
    tool_call_count: usize,
    tool_names: Vec<String>,
    usage: Option<(i32, i32, i32)>,
) -> LlmLogOutput {
    let (prompt_tokens, completion_tokens, total_tokens) = usage.unwrap_or((0, 0, 0));
    LlmLogOutput {
        content: truncate(content),
        tool_call_count,
        tool_names,
        usage: UsageSummary {
            prompt_tokens,
            completion_tokens,
            total_tokens,
        },
    }
}

/// Helper struct for building tool call entries.
#[derive(Debug, Default)]
pub struct ToolCallEntry {
    pub user_id: String,
    pub tool: String,
    pub duration_ms: u64,
    pub input: String,
    pub output: String,
}

impl ToolCallEntry {
    /// Create a new tool call entry builder.
    pub fn new(user_id: &str, tool: &str) -> Self {
        ToolCallEntry {
            user_id: user_id.to_string(),
            tool: tool.to_string(),
            ..Default::default()
        }
    }

    /// Build the final LogEntry with the given input, output, and duration.
    pub fn into_log_entry_with(self, input: &str, output: &str, duration_ms: u64) -> LogEntry {
        LogEntry::ToolCall {
            timestamp: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            user_id: self.user_id,
            tool: self.tool,
            duration_ms,
            input: truncate(input),
            output: truncate(output),
        }
    }

    /// Build the final LogEntry.
    pub fn into_log_entry(self) -> LogEntry {
        LogEntry::ToolCall {
            timestamp: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            user_id: self.user_id,
            tool: self.tool,
            duration_ms: self.duration_ms,
            input: truncate(&self.input),
            output: truncate(&self.output),
        }
    }
}
