//! Skill tool - expose skill operations to the LLM.

use anyhow::Result;
use async_trait::async_trait;

use crate::base::{Tool, ToolContext};

/// View a skill's content.
pub struct SkillViewTool;

#[async_trait]
impl Tool for SkillViewTool {
    fn name(&self) -> &str { "skill_view" }
    fn description(&self) -> &str { "View the content of a skill by name. Load before using a skill for best results." }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "Skill name to view" }
            },
            "required": ["name"]
        })
    }
    async fn execute(&self, _args: &str, _ctx: &ToolContext) -> Result<String> {
        Ok("Skill view (stub)".to_string())
    }
}

/// Manage skills - create, patch, update.
pub struct SkillManageTool;

#[async_trait]
impl Tool for SkillManageTool {
    fn name(&self) -> &str { "skill_manage" }
    fn description(&self) -> &str { "Create or patch a skill. After completing complex tasks, save the approach as a skill. If a skill is outdated, patch it immediately." }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["create", "patch", "delete"], "description": "Action to perform" },
                "name": { "type": "string", "description": "Skill name" },
                "description": { "type": "string", "description": "Short description of the skill" },
                "content": { "type": "string", "description": "Skill content (markdown)" },
                "patch_instructions": { "type": "string", "description": "For patch action: what to change" }
            },
            "required": ["action", "name"]
        })
    }
    async fn execute(&self, _args: &str, _ctx: &ToolContext) -> Result<String> {
        Ok("Skill manage (stub)".to_string())
    }
}

/// Search skills.
pub struct SkillSearchTool;

#[async_trait]
impl Tool for SkillSearchTool {
    fn name(&self) -> &str { "skill_search" }
    fn description(&self) -> &str { "Search available skills by name or description." }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Search query" }
            },
            "required": ["query"]
        })
    }
    async fn execute(&self, _args: &str, _ctx: &ToolContext) -> Result<String> {
        Ok("Skill search (stub)".to_string())
    }
}
