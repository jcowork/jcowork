//! Todo tool - manage todo lists.

use anyhow::Result;
use async_trait::async_trait;

use crate::base::{Tool, ToolContext};

/// Add a todo item.
pub struct TodoAddTool;

#[async_trait]
impl Tool for TodoAddTool {
    fn name(&self) -> &str { "todo_add" }
    fn description(&self) -> &str { "Add an item to the todo list." }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "content": { "type": "string", "description": "Todo item content" }
            },
            "required": ["content"]
        })
    }
    async fn execute(&self, _args: &str, _ctx: &ToolContext) -> Result<String> {
        Ok("Todo added (stub)".to_string())
    }
}

/// List todo items.
pub struct TodoListTool;

#[async_trait]
impl Tool for TodoListTool {
    fn name(&self) -> &str { "todo_list" }
    fn description(&self) -> &str { "List all todo items." }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object", "properties": {} })
    }
    async fn execute(&self, _args: &str, _ctx: &ToolContext) -> Result<String> {
        Ok("Todo list (stub)".to_string())
    }
}

/// Complete a todo item.
pub struct TodoCompleteTool;

#[async_trait]
impl Tool for TodoCompleteTool {
    fn name(&self) -> &str { "todo_complete" }
    fn description(&self) -> &str { "Mark a todo item as completed." }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "Todo item ID to complete" }
            },
            "required": ["id"]
        })
    }
    async fn execute(&self, _args: &str, _ctx: &ToolContext) -> Result<String> {
        Ok("Todo completed (stub)".to_string())
    }
}
