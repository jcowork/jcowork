//! Delegate tool - spawn subagent for parallel work.

use anyhow::Result;
use async_trait::async_trait;

use crate::base::{Tool, ToolContext};

/// Delegate tool that spawns a sub-agent task.
pub struct DelegateTool;

#[async_trait]
impl Tool for DelegateTool {
    fn name(&self) -> &str { "delegate" }
    fn description(&self) -> &str { "Spawn a sub-agent to handle a specific task in parallel. The sub-agent has access to all tools and works independently." }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "task": { "type": "string", "description": "Description of the task for the sub-agent" },
                "workspace": { "type": "string", "description": "Working directory for the sub-agent (default: current workspace)" }
            },
            "required": ["task"]
        })
    }
    async fn execute(&self, _args: &str, _ctx: &ToolContext) -> Result<String> {
        // Sub-agent spawning is handled by the AgentLoop
        Ok("Delegate (stub - handled by AgentLoop)".to_string())
    }
}
