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

/// One message event emitted by `lark-cli event consume` (NDJSON).
///
/// The CLI already flattens the event into a small object (chat_id,
/// message_id, pre-rendered text content, …), but older builds wrap the raw
/// Feishu v2 payload instead. [`parse_cli_event`] accepts both shapes.
#[derive(Debug, Clone, PartialEq)]
pub struct BotEvent {
    pub message_id: String,
    pub chat_id: String,
    pub chat_type: String,
    pub message_type: String,
    pub content: String,
    pub sender_type: Option<String>,
    pub mentions: Vec<serde_json::Value>,
}

impl BotEvent {
    /// Group chats only get an answer when the bot was mentioned — the
    /// official CLI guide warns against acting on every group message.
    /// Messages from other bots are ignored so two bots cannot loop.
    pub fn should_answer(&self) -> bool {
        if self.sender_type.as_deref() == Some("bot") {
            return false;
        }
        if self.chat_type != "p2p" && self.mentions.is_empty() {
            return false;
        }
        // Only text is actionable: the agent cannot see images or files, and
        // the CLI's pre-rendered content for those is a placeholder.
        matches!(self.message_type.as_str(), "text" | "post" | "")
            && !self.content.trim().is_empty()
    }
}

/// Parse one stdout line produced by `lark-cli event consume`.
///
/// Returns `None` for non-message bus lines (ready markers, errors, …), so a
/// caller can feed every line in without filtering first.
pub fn parse_cli_event(line: &str) -> Option<BotEvent> {
    let line = line.trim();
    if !line.starts_with('{') {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(line).ok()?;

    // Skip anything that is clearly not a message event.
    if let Some(kind) = value.get("type").and_then(|v| v.as_str()) {
        if kind != "im.message.receive_v1" {
            return None;
        }
    }

    let root = value
        .get("event")
        .filter(|v| v.is_object())
        .or_else(|| value.get("data").filter(|v| v.is_object()))
        .unwrap_or(&value);

    // Flattened CLI shape keeps message fields on the root; raw Feishu v2
    // nests them under `message` with the sender beside it.
    let message = root.get("message").unwrap_or(root);

    let message_id = json_str(message, "message_id")
        .or_else(|| json_str(message, "id"))?;
    let chat_id = json_str(message, "chat_id")?;
    let message_type = json_str(message, "message_type").unwrap_or_default();
    let content = normalize_content(&json_str(message, "content").unwrap_or_default());
    let chat_type = json_str(message, "chat_type").unwrap_or_else(|| "p2p".to_string());
    let sender_type = root
        .get("sender")
        .and_then(|s| s.get("sender_type"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| json_str(root, "sender_type"));
    let mentions = root
        .get("mentions")
        .or_else(|| message.get("mentions"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    Some(BotEvent {
        message_id,
        chat_id,
        chat_type,
        message_type,
        content,
        sender_type,
        mentions,
    })
}

fn json_str(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Raw events carry the message content as a JSON string (`{"text":"hi"}`);
/// the CLI's flattened shape carries plain text already.
fn normalize_content(content: &str) -> String {
    let trimmed = content.trim();
    if trimmed.starts_with('{') {
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(trimmed) {
            if let Some(text) = parsed.get("text").and_then(|v| v.as_str()) {
                return text.to_string();
            }
        }
    }
    content.to_string()
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

    #[test]
    fn test_parse_cli_event_flattened() {
        let line = r#"{"type":"im.message.receive_v1","message_id":"om_1","chat_id":"oc_1","chat_type":"p2p","message_type":"text","content":"你好","sender_type":"user"}"#;
        let ev = parse_cli_event(line).expect("event");
        assert_eq!(ev.message_id, "om_1");
        assert_eq!(ev.chat_id, "oc_1");
        assert_eq!(ev.content, "你好");
        assert_eq!(ev.chat_type, "p2p");
        assert!(ev.should_answer());
    }

    #[test]
    fn test_parse_cli_event_raw_v2() {
        let line = r#"{"schema":"2.0","header":{"event_type":"im.message.receive_v1"},"event":{"sender":{"sender_type":"user"},"message":{"message_id":"om_2","chat_id":"oc_2","chat_type":"group","message_type":"text","content":"{\"text\":\"hi\"}"}}}"#;
        let ev = parse_cli_event(line).expect("event");
        assert_eq!(ev.content, "hi");
        assert_eq!(ev.sender_type.as_deref(), Some("user"));
        // Group message without a mention is ignored.
        assert!(!ev.should_answer());
    }

    #[test]
    fn test_parse_cli_event_skips_lifecycle_lines() {
        assert!(parse_cli_event(r#"{"type":"ready","ok":true}"#).is_none());
        assert!(parse_cli_event("[event] connecting...").is_none());
    }

    #[test]
    fn test_should_answer_ignores_bot_and_non_text() {
        let mut ev = BotEvent {
            message_id: "om".into(),
            chat_id: "oc".into(),
            chat_type: "p2p".into(),
            message_type: "text".into(),
            content: "hi".into(),
            sender_type: Some("user".into()),
            mentions: vec![],
        };
        assert!(ev.should_answer());
        ev.sender_type = Some("bot".into());
        assert!(!ev.should_answer());
        ev.sender_type = Some("user".into());
        ev.message_type = "image".into();
        assert!(!ev.should_answer());
        ev.message_type = "text".into();
        ev.content = "   ".into();
        assert!(!ev.should_answer());
    }
}
