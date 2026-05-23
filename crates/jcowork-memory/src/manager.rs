//! Memory Manager - orchestrates memory providers.

use anyhow::Result;
use std::sync::Arc;

use crate::models::{MemoryEntry, MemorySearchResult};
use crate::provider::MemoryProvider;

/// Orchestrates memory providers for the agent.
///
/// Supports one builtin provider plus at most one external provider.
/// The builtin provider is always first; failures in one never block the other.
pub struct MemoryManager {
    providers: Vec<Arc<dyn MemoryProvider>>,
    has_external: bool,
}

impl MemoryManager {
    pub fn new() -> Self {
        Self {
            providers: Vec::new(),
            has_external: false,
        }
    }

    /// Add a memory provider. Only one external provider is allowed.
    pub fn add_provider(&mut self, provider: Arc<dyn MemoryProvider>) {
        let is_builtin = provider.name() == "builtin";
        if !is_builtin {
            if self.has_external {
                tracing::warn!(
                    "Rejected external memory provider '{}' — only one external provider allowed",
                    provider.name()
                );
                return;
            }
            self.has_external = true;
        }
        tracing::info!(provider = %provider.name(), "Registered memory provider");
        self.providers.push(provider);
    }

    /// Save a memory entry via the first provider.
    pub async fn save(&self, user_id: &str, content: &str, category: &str) -> Result<MemoryEntry> {
        if let Some(provider) = self.providers.first() {
            return provider.save(user_id, content, category).await;
        }
        anyhow::bail!("No memory provider registered");
    }

    /// Recall all memories from all providers.
    pub async fn recall_all(&self, user_id: &str) -> Result<Vec<MemoryEntry>> {
        let mut all = Vec::new();
        for provider in &self.providers {
            match provider.recall_all(user_id).await {
                Ok(entries) => all.extend(entries),
                Err(e) => tracing::warn!(provider = %provider.name(), err = %e, "Memory recall failed"),
            }
        }
        Ok(all)
    }

    /// Search memories from all providers.
    pub async fn search(&self, user_id: &str, query: &str, limit: usize) -> Result<Vec<MemorySearchResult>> {
        let mut all = Vec::new();
        for provider in &self.providers {
            match provider.search(user_id, query, limit).await {
                Ok(results) => all.extend(results),
                Err(e) => tracing::warn!(provider = %provider.name(), err = %e, "Memory search failed"),
            }
        }
        all.sort_by(|a, b| b.rank.partial_cmp(&a.rank).unwrap_or(std::cmp::Ordering::Equal));
        all.truncate(limit);
        Ok(all)
    }

    /// Delete a memory from all providers.
    pub async fn delete(&self, user_id: &str, memory_id: &str) -> Result<()> {
        for provider in &self.providers {
            if let Err(e) = provider.delete(user_id, memory_id).await {
                tracing::warn!(provider = %provider.name(), err = %e, "Memory delete failed");
            }
        }
        Ok(())
    }

    /// Update a memory via the first provider.
    pub async fn update(&self, user_id: &str, memory_id: &str, content: Option<&str>, category: Option<&str>) -> Result<MemoryEntry> {
        if let Some(provider) = self.providers.first() {
            return provider.update(user_id, memory_id, content, category).await;
        }
        anyhow::bail!("No memory provider registered");
    }

    /// Build combined system prompt block from all providers.
    pub async fn build_system_prompt(&self, user_id: &str) -> String {
        let mut blocks = Vec::new();
        for provider in &self.providers {
            let block = provider.system_prompt_block(user_id).await;
            if !block.is_empty() {
                blocks.push(format!("[{}]\n{}", provider.name(), block));
            }
        }
        blocks.join("\n\n")
    }
}

impl Default for MemoryManager {
    fn default() -> Self {
        Self::new()
    }
}
