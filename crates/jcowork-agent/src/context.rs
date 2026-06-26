//! Context Engine trait and built-in Compressor.

use anyhow::Result;
use async_trait::async_trait;
use jcowork_llm::provider::{ChatMessage, Usage};

/// Trait for context compression engines.
///
/// Context compression engine.
/// Controls how conversation context is managed when approaching the
/// model's token limit.
#[async_trait]
pub trait ContextEngine: Send + Sync {
    /// Engine name.
    fn name(&self) -> &str;

    /// Update tracked token usage from an API response.
    fn update_from_response(&mut self, usage: &Usage);

    /// Check if compression should fire this turn.
    fn should_compress(&self, prompt_tokens: i32) -> bool;

    /// Compress the message list and return the new (possibly shorter) list.
    async fn compress(
        &self,
        messages: Vec<ChatMessage>,
        current_tokens: i32,
    ) -> Result<Vec<ChatMessage>>;

    /// Get the context length for the current model.
    fn context_length(&self) -> usize;
}

/// Built-in context compressor using LLM summarization.
///
/// When the conversation approaches the model's token limit,
/// older messages are summarized into a compact form.
pub struct Compressor {
    context_length: usize,
    threshold_percent: f32,
    protect_first_n: usize,
    protect_last_n: usize,
    last_prompt_tokens: i32,
    #[allow(dead_code)]
    compression_count: usize,
}

impl Compressor {
    pub fn new(context_length: usize) -> Self {
        Self {
            context_length,
            threshold_percent: 0.75,
            protect_first_n: 3,
            protect_last_n: 6,
            last_prompt_tokens: 0,
            compression_count: 0,
        }
    }

    pub fn with_threshold(mut self, percent: f32) -> Self {
        self.threshold_percent = percent;
        self
    }
}

#[async_trait]
impl ContextEngine for Compressor {
    fn name(&self) -> &str {
        "compressor"
    }

    fn update_from_response(&mut self, usage: &Usage) {
        self.last_prompt_tokens = usage.prompt_tokens;
    }

    fn should_compress(&self, prompt_tokens: i32) -> bool {
        let threshold = (self.context_length as f32 * self.threshold_percent) as i32;
        prompt_tokens > threshold
    }

    async fn compress(
        &self,
        messages: Vec<ChatMessage>,
        _current_tokens: i32,
    ) -> Result<Vec<ChatMessage>> {
        // Simple compression: protect first N and last N messages,
        // replace middle with a summary placeholder.
        if messages.len() <= self.protect_first_n + self.protect_last_n {
            return Ok(messages);
        }

        let mut compressed = Vec::new();

        // Keep first N messages (system + early context)
        for msg in messages.iter().take(self.protect_first_n) {
            compressed.push(msg.clone());
        }

        // Add compression marker as assistant message (not system)
        // to avoid "System message must be at the beginning" errors
        compressed.push(ChatMessage {
            role: "assistant".to_string(),
            content: format!(
                "[Context compressed: {} messages summarized. {} messages preserved.]",
                messages.len() - self.protect_first_n - self.protect_last_n,
                self.protect_first_n + self.protect_last_n
            ),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        });

        // Keep last N messages (recent context)
        let start = messages.len() - self.protect_last_n;
        for msg in messages.iter().skip(start) {
            compressed.push(msg.clone());
        }

        Ok(compressed)
    }

    fn context_length(&self) -> usize {
        self.context_length
    }
}
