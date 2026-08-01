//! Tool trait definition.

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// A tool's JSON schema parameter definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolParameter {
    pub name: String,
    #[serde(rename = "type")]
    pub param_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r#enum: Option<Vec<String>>,
}

/// Context provided to tool execution (user-scoped).
#[derive(Debug, Clone)]
pub struct ToolContext {
    pub user_id: String,
    pub workspace_root: String,
}

/// Truncate a string to at most `max_bytes` bytes without splitting a
/// multi-byte UTF-8 character (plain `&s[..max_bytes]` panics on e.g. Chinese text).
pub fn truncate_str(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Trait that all tools must implement.
///
/// Each tool provides:
/// - A name and description for LLM discovery
/// - A JSON schema for parameter validation
/// - An async execute method that takes arguments and returns a string result
#[async_trait]
pub trait Tool: Send + Sync {
    /// Unique tool name (e.g., "shell", "file_read").
    fn name(&self) -> &str;

    /// Human-readable description shown to the LLM.
    fn description(&self) -> &str;

    /// JSON Schema for the tool's parameters.
    fn parameters(&self) -> serde_json::Value;

    /// Execute the tool with the given arguments and context.
    async fn execute(&self, args: &str, ctx: &ToolContext) -> Result<String>;
}

#[cfg(test)]
mod tests {
    use super::truncate_str;

    #[test]
    fn test_truncate_str_respects_char_boundary() {
        // '再' is 3 bytes (498..501 in the original panic); byte index 500 is mid-char
        let s = "陈太丘与友期行，期日中。过中不至，太丘舍去，去后乃至。";
        let truncated = truncate_str(s, 10);
        assert!(truncated.len() <= 10);
        assert!(s.starts_with(truncated));

        // ASCII is unaffected
        assert_eq!(truncate_str("hello world", 5), "hello");

        // Short strings pass through unchanged
        assert_eq!(truncate_str("短", 500), "短");
    }
}
