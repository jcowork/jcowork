//! Memory tool - expose memory operations to the LLM.

use anyhow::Result;
use async_trait::async_trait;
use jcowork_memory::MemoryManager;
use std::sync::Arc;

use crate::base::{Tool, ToolContext};

/// Memory save tool.
pub struct MemorySaveTool {
    pub manager: Arc<MemoryManager>,
}

#[async_trait]
impl Tool for MemorySaveTool {
    fn name(&self) -> &str { "memory_save" }
    fn description(&self) -> &str {
        "Save a durable fact or life event to persistent memory. \
         Use for: user preferences, environment details, conventions, \
         AND daily life events (e.g., dropping kids at school, dining with someone, visiting a place, completing a task). \
         For life events, use category='life_event' and include timestamp in content."
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "content": {
                    "type": "string",
                    "description": "The fact or event to save as a declarative statement. For life events, include time context, e.g.: '2026-05-21 08:30 送孩子去学校' or '2026-05-21 午饭 和张总在望京吃火锅'"
                },
                "category": {
                    "type": "string",
                    "description": "Category for this memory. Use: 'life_event' for daily activities/events, 'preference' for user preferences, 'environment' for env/tool facts, 'convention' for coding/work conventions, 'person' for info about people, 'general' as fallback.",
                    "enum": ["life_event", "preference", "environment", "convention", "person", "general"],
                    "default": "general"
                }
            },
            "required": ["content"]
        })
    }
    async fn execute(&self, args: &str, ctx: &ToolContext) -> Result<String> {
        let params: serde_json::Value = serde_json::from_str(args)?;
        let content = params["content"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'content' parameter"))?;
        let category = params["category"]
            .as_str()
            .unwrap_or("general");

        match self.manager.save(&ctx.user_id, content, category).await {
            Ok(entry) => Ok(format!("Memory saved: [{}] {}", entry.category, entry.content)),
            Err(e) => Ok(format!("Failed to save memory: {}", e)),
        }
    }
}

/// Memory recall tool.
pub struct MemoryRecallTool {
    pub manager: Arc<MemoryManager>,
}

#[async_trait]
impl Tool for MemoryRecallTool {
    fn name(&self) -> &str { "memory_recall" }
    fn description(&self) -> &str { "Recall all saved memories. Returns the full memory context." }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object", "properties": {} })
    }
    async fn execute(&self, _args: &str, ctx: &ToolContext) -> Result<String> {
        match self.manager.recall_all(&ctx.user_id).await {
            Ok(memories) if memories.is_empty() => Ok("No memories saved yet.".to_string()),
            Ok(memories) => {
                let lines: Vec<String> = memories
                    .iter()
                    .map(|m| format!("- [{}] {}", m.category, m.content))
                    .collect();
                Ok(format!("Memories:\n{}", lines.join("\n")))
            }
            Err(e) => Ok(format!("Failed to recall memories: {}", e)),
        }
    }
}

/// Memory search tool.
pub struct MemorySearchTool {
    pub manager: Arc<MemoryManager>,
}

#[async_trait]
impl Tool for MemorySearchTool {
    fn name(&self) -> &str { "memory_search" }
    fn description(&self) -> &str {
        "Search memories using full-text search. Use when you need specific past knowledge."
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Search query" },
                "limit": { "type": "integer", "description": "Max results (default: 5)", "default": 5 }
            },
            "required": ["query"]
        })
    }
    async fn execute(&self, args: &str, ctx: &ToolContext) -> Result<String> {
        let params: serde_json::Value = serde_json::from_str(args)?;
        let query = params["query"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'query' parameter"))?;
        let limit = params["limit"].as_u64().unwrap_or(5) as usize;

        match self.manager.search(&ctx.user_id, query, limit).await {
            Ok(results) if results.is_empty() => Ok("No matching memories found.".to_string()),
            Ok(results) => {
                let lines: Vec<String> = results
                    .iter()
                    .map(|r| format!("- [{}] {} (score: {:.2})", r.category, r.content, r.rank))
                    .collect();
                Ok(format!("Search results:\n{}", lines.join("\n")))
            }
            Err(e) => Ok(format!("Failed to search memories: {}", e)),
        }
    }
}
