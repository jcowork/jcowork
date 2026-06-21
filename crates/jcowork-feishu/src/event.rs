//! Feishu event deserialization and dispatch.

use serde::Deserialize;

/// Top-level Feishu event wrapper.
#[derive(Debug, Clone, Deserialize)]
pub struct FeishuEvent {
    pub schema: Option<String>,
    pub header: Option<EventHeader>,
    pub event: Option<EventPayload>,
    /// Challenge field (present during URL verification).
    pub challenge: Option<String>,
    /// Token field (present during URL verification).
    pub token: Option<String>,
    pub event_type: Option<String>,
}

/// Event header with metadata.
#[derive(Debug, Clone, Deserialize)]
pub struct EventHeader {
    pub event_id: Option<String>,
    pub event_type: Option<String>,
    pub create_time: Option<String>,
    pub token: Option<String>,
    pub app_id: Option<String>,
    /// Tenant key for ISV apps.
    pub tenant_key: Option<String>,
}

/// Inner event payload for im.message.receive_v1.
#[derive(Debug, Clone, Deserialize)]
pub struct EventPayload {
    pub sender: EventSender,
    pub message: EventMessage,
}

/// Sender information.
#[derive(Debug, Clone, Deserialize)]
pub struct EventSender {
    pub sender_id: SenderId,
    pub sender_type: Option<String>,
}

/// Sender ID with multiple ID types.
#[derive(Debug, Clone, Deserialize)]
pub struct SenderId {
    pub open_id: Option<String>,
    pub user_id: Option<String>,
    pub union_id: Option<String>,
}

/// Message information.
#[derive(Debug, Clone, Deserialize)]
pub struct EventMessage {
    pub message_id: Option<String>,
    pub chat_id: Option<String>,
    pub chat_type: Option<String>,
    pub message_type: Option<String>,
    pub content: Option<String>,
    pub create_time: Option<String>,
}

/// Parsed message ready for processing.
#[derive(Debug, Clone)]
pub struct ParsedMessage {
    pub open_id: String,
    pub chat_id: String,
    pub message_id: String,
    pub text: String,
}

impl FeishuEvent {
    /// Check if this is a challenge/verification request.
    pub fn is_challenge(&self) -> bool {
        self.challenge.is_some()
    }

    /// Parse the event into a ParsedMessage for agent processing.
    pub fn parse_message(&self) -> Option<ParsedMessage> {
        let event = self.event.as_ref()?;
        let open_id = event.sender.sender_id.open_id.clone()?;
        let chat_id = event.message.chat_id.clone()?;
        let message_id = event.message.message_id.clone()?;
        let msg_type = event.message.message_type.as_deref().unwrap_or("");

        // Only handle text messages for now
        if msg_type != "text" {
            return None;
        }

        let content = event.message.content.as_deref().unwrap_or("{}");
        let text = extract_text_from_content(content)?;

        Some(ParsedMessage {
            open_id,
            chat_id,
            message_id,
            text,
        })
    }
}

/// Extract plain text from the Feishu message content JSON.
/// Text message content format: {"text":"hello"}
fn extract_text_from_content(content: &str) -> Option<String> {
    let parsed: serde_json::Value = serde_json::from_str(content).ok()?;
    let text = parsed.get("text")?.as_str()?;
    // Strip @bot mention prefix if present (e.g., "@_user_1 hello" → "hello")
    let cleaned = text
        .trim()
        .trim_start_matches(|c: char| c == '@')
        .trim_start_matches(|c: char| c.is_alphanumeric() || c == '_')
        .trim();
    Some(if cleaned.is_empty() { text.trim().to_string() } else { cleaned.to_string() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_text() {
        let content = r#"{"text":"hello"}"#;
        assert_eq!(extract_text_from_content(content), Some("hello".to_string()));
    }

    #[test]
    fn test_extract_text_with_mention() {
        let content = r#"{"text":"@_user_1 hello world"}"#;
        assert_eq!(extract_text_from_content(content), Some("hello world".to_string()));
    }
}
