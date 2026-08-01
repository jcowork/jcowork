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

    /// Dispatch a tool call in a separate task so a panicking tool unwinds
    /// only that task instead of killing the caller's agent loop.
    ///
    /// A panic inside tool code is converted into a normal `Err` result,
    /// letting the loop report the failure and continue. Prefer this over
    /// [`ToolRegistry::dispatch`] when driving an LLM agent loop.
    pub async fn dispatch_isolated(self: &Arc<Self>, name: &str, args: &str, ctx: &ToolContext) -> Result<String> {
        let registry = Arc::clone(self);
        let name_owned = name.to_string();
        let args_owned = args.to_string();
        let ctx_owned = ctx.clone();

        let handle = tokio::spawn(async move {
            registry.dispatch(&name_owned, &args_owned, &ctx_owned).await
        });

        match handle.await {
            Ok(result) => result,
            Err(e) => {
                tracing::error!(tool = %name, err = %e, "Tool execution crashed");
                Err(anyhow!("Tool '{}' crashed: {}", name, e))
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;

    struct PanicTool;

    #[async_trait]
    impl Tool for PanicTool {
        fn name(&self) -> &str {
            "panic_tool"
        }
        fn description(&self) -> &str {
            ""
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }
        async fn execute(&self, _args: &str, _ctx: &ToolContext) -> Result<String> {
            panic!("boom");
        }
    }

    #[tokio::test]
    async fn test_dispatch_isolated_survives_panicking_tool() {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(PanicTool));
        let registry = Arc::new(registry);

        let ctx = ToolContext {
            user_id: "u".to_string(),
            workspace_root: "/tmp".to_string(),
        };

        // A panicking tool becomes an Err instead of killing the caller
        let result = registry.dispatch_isolated("panic_tool", "{}", &ctx).await;
        let err = result.unwrap_err().to_string();
        assert!(err.contains("crashed"), "unexpected error: {}", err);

        // Unknown tools are still reported normally
        let result = registry.dispatch_isolated("missing_tool", "{}", &ctx).await;
        assert!(result.unwrap_err().to_string().contains("Unknown tool"));
    }
}
