//! Memory Provider trait definition.

use anyhow::Result;
use async_trait::async_trait;

use crate::models::{MemoryEntry, MemorySearchResult};

/// Trait that all memory providers must implement.
///
/// The builtin provider uses SQLite FTS5; external providers can be plugged in.
#[async_trait]
pub trait MemoryProvider: Send + Sync {
    /// Provider name (e.g., "builtin", "honcho").
    fn name(&self) -> &str;

    /// Save a memory entry.
    async fn save(&self, user_id: &str, content: &str, category: &str) -> Result<MemoryEntry>;

    /// Recall all memories for a user (for system prompt injection).
    async fn recall_all(&self, user_id: &str) -> Result<Vec<MemoryEntry>>;

    /// Search memories using full-text search.
    async fn search(&self, user_id: &str, query: &str, limit: usize) -> Result<Vec<MemorySearchResult>>;

    /// Delete a memory by ID.
    async fn delete(&self, user_id: &str, memory_id: &str) -> Result<()>;

    /// Update a memory's content and/or category.
    async fn update(&self, user_id: &str, memory_id: &str, content: Option<&str>, category: Option<&str>) -> Result<MemoryEntry>;

    /// Build the system prompt block from memories.
    /// This gets injected into the agent's system prompt.
    async fn system_prompt_block(&self, user_id: &str) -> String {
        match self.recall_all(user_id).await {
            Ok(memories) if memories.is_empty() => String::new(),
            Ok(memories) => {
                let memory_text = memories
                    .iter()
                    .map(|m| format!("- [{}] {}", m.category, m.content))
                    .collect::<Vec<_>>()
                    .join("\n");

                format!(
                    "<memory-context>\n\
                     [System note: The following is recalled memory context, \
                     NOT new user input. Treat as authoritative reference data — \
                     this is the agent's persistent memory.]\n\n\
                     {memory_text}\n\
                     </memory-context>"
                )
            }
            Err(e) => {
                tracing::warn!("Failed to load memory for system prompt: {}", e);
                String::new()
            }
        }
    }
}
