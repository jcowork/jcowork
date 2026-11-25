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
        /// Whether the call succeeded (one kind of failure is reported through
        /// `output` text alone, so readers must not guess from the text when
        /// this field is present).
        ok: bool,
        /// Machine-readable failure kind (e.g. "timeout", "reported");
        /// absent on success and on logs written before this field existed.
        #[serde(skip_serializing_if = "Option::is_none")]
        error_kind: Option<String>,
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
    /// A full-duplex voice call pipeline milestone (ASR / reply / TTS),
    /// written by the gateway voice channel for end-to-end latency attribution.
    #[serde(rename = "voice_stage")]
    VoiceStage {
        /// ISO 8601 timestamp of when the stage was observed.
        timestamp: String,
        /// User ID that owns the call.
        user_id: String,
        /// Conversation ID the call belongs to.
        conv_id: String,
        /// Stage name (e.g., "asr_final", "tts_first_audio", "session_end").
        stage: String,
        /// Milliseconds since the call WebSocket session started.
        elapsed_ms: u64,
        /// Stage-specific payload (transcript, byte counts, durations, errors).
        data: serde_json::Value,
    },
}

/// Builder for a voice pipeline stage entry (timestamp = now).
pub fn build_voice_stage(
    user_id: &str,
    conv_id: &str,
    stage: &str,
    elapsed_ms: u64,
    data: serde_json::Value,
) -> LogEntry {
    LogEntry::VoiceStage {
        timestamp: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        user_id: user_id.to_string(),
        conv_id: conv_id.to_string(),
        stage: stage.to_string(),
        elapsed_ms,
        data,
    }
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
#[derive(Debug)]
pub struct ToolCallEntry {
    pub user_id: String,
    pub tool: String,
    pub duration_ms: u64,
    pub input: String,
    pub output: String,
    pub ok: bool,
    pub error_kind: Option<String>,
}

impl Default for ToolCallEntry {
    fn default() -> Self {
        Self {
            user_id: String::new(),
            tool: String::new(),
            duration_ms: 0,
            input: String::new(),
            output: String::new(),
            // A default-built entry has no known failure to report.
            ok: true,
            error_kind: None,
        }
    }
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

    /// Mark the call as failed with the given error kind.
    pub fn with_failure(mut self, error_kind: &str) -> Self {
        self.ok = false;
        self.error_kind = Some(error_kind.to_string());
        self
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
            ok: self.ok,
            error_kind: self.error_kind,
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
            ok: self.ok,
            error_kind: self.error_kind,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voice_stage_serializes_with_elapsed_and_payload() {
        let entry = build_voice_stage(
            "u1",
            "c1",
            "tts_first_audio",
            1234,
            serde_json::json!({"ttfa_ms": 430, "bytes": 9600}),
        );
        let json = serde_json::to_value(&entry).unwrap();
        assert_eq!(json["event"], "voice_stage");
        assert_eq!(json["stage"], "tts_first_audio");
        assert_eq!(json["conv_id"], "c1");
        assert_eq!(json["elapsed_ms"], 1234);
        assert_eq!(json["data"]["ttfa_ms"], 430);
    }

    #[test]
    fn failed_tool_call_serializes_structured_verdict() {
        let entry = ToolCallEntry::new("u1", "web_search")
            .with_failure("timeout")
            .into_log_entry_with("{}", "web_search: timed out after 60s", 60_000);

        let json = serde_json::to_value(&entry).unwrap();
        assert_eq!(json["event"], "tool_call");
        assert_eq!(json["ok"], false);
        assert_eq!(json["error_kind"], "timeout");
        assert_eq!(json["duration_ms"], 60_000);
    }

    #[test]
    fn successful_tool_call_keeps_the_old_field_set() {
        let entry = ToolCallEntry::new("u1", "file_read")
            .into_log_entry_with("{}", "content", 5);

        let json = serde_json::to_value(&entry).unwrap();
        assert_eq!(json["ok"], true);
        assert!(
            json.get("error_kind").is_none(),
            "success must not carry a failure kind"
        );
    }
}
