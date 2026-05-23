//! Tool Registry - dynamic registration and dispatch.

use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::sync::Arc;

use crate::base::{Tool, ToolContext};
use jcowork_llm::provider::{FunctionDefinition, ToolDefinition};

/// Registry of available tools.
///
/// Tools are registered at startup and dispatched by name when the LLM
/// returns a tool_call. Tools are registered by name and dispatched via trait objects.
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    /// Register a tool.
    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        let name = tool.name().to_string();
        tracing::info!(tool = %name, "Registered tool");
        self.tools.insert(name, tool);
    }

    /// Get a tool by name.
    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    /// Dispatch a tool call by name with arguments.
    pub async fn dispatch(&self, name: &str, args: &str, ctx: &ToolContext) -> Result<String> {
        let tool = self
            .tools
            .get(name)
            .ok_or_else(|| anyhow!("Unknown tool: {}", name))?;
        tool.execute(args, ctx).await
    }

    /// Get all tool definitions in OpenAI function-calling format.
    pub fn all_schemas(&self) -> Vec<ToolDefinition> {
        self.tools
            .values()
            .map(|tool| ToolDefinition {
                r#type: "function".to_string(),
                function: FunctionDefinition {
                    name: tool.name().to_string(),
                    description: tool.description().to_string(),
                    parameters: tool.parameters(),
                },
            })
            .collect()
    }

    /// List all registered tool names.
    pub fn tool_names(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }

    /// Check if a tool is registered.
    pub fn has(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}
