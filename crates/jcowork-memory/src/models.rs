//! Memory data models.

use serde::{Deserialize, Serialize};

/// A single memory entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub id: String,
    pub user_id: String,
    pub content: String,
    pub category: String,
    pub created_at: String,
    pub updated_at: String,
}

/// A memory search result with relevance score.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemorySearchResult {
    pub id: String,
    pub content: String,
    pub category: String,
    pub rank: f64,
}
