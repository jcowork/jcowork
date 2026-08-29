//! Tool Registry - dynamic registration and dispatch.

use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use crate::base::{Tool, ToolContext};
use jcowork_llm::provider::{FunctionDefinition, ToolDefinition};

/// Registry of available tools.
///
/// Tools are registered at startup and dispatched by name when the LLM
/// returns a tool_call. Static tools are registered by name at construction
/// time; dynamic tools (e.g. connector tools) can be registered or removed at
/// runtime through [`ToolRegistry::register_dynamic`] /
/// [`ToolRegistry::unregister_dynamic`].
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
    /// Tools registered at runtime (connector tools etc.).
    dynamic: RwLock<HashMap<String, Arc<dyn Tool>>>,
}

impl ToolRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
            dynamic: RwLock::new(HashMap::new()),
        }
    }

    /// Register a tool.
    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        let name = tool.name().to_string();
        tracing::info!(tool = %name, "Registered tool");
        self.tools.insert(name, tool);
    }

    /// Register a tool at runtime (no exclusive access required).
    ///
    /// Used for dynamically discovered tools such as connector tools.
    /// Re-registering the same name replaces the previous instance.
    pub fn register_dynamic(&self, tool: Arc<dyn Tool>) {
        let name = tool.name().to_string();
        tracing::info!(tool = %name, "Registered dynamic tool");
        self.dynamic.write().unwrap().insert(name, tool);
    }

    /// Remove a dynamically registered tool. No-op for unknown names.
    pub fn unregister_dynamic(&self, name: &str) {
        if self.dynamic.write().unwrap().remove(name).is_some() {
            tracing::info!(tool = %name, "Unregistered dynamic tool");
        }
    }

    /// Get a tool by name (static tools take precedence).
    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools
            .get(name)
            .cloned()
            .or_else(|| self.dynamic.read().unwrap().get(name).cloned())
    }

    /// Dispatch a tool call by name with arguments.
    pub async fn dispatch(&self, name: &str, args: &str, ctx: &ToolContext) -> Result<String> {
        let tool = self.get(name).ok_or_else(|| anyhow!("Unknown tool: {}", name))?;
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

    /// Get all tool definitions in OpenAI function-calling format
    /// (static + dynamic tools).
    pub fn all_schemas(&self) -> Vec<ToolDefinition> {
        let mut schemas: Vec<ToolDefinition> = self
            .tools
            .values()
            .chain(self.dynamic.read().unwrap().values())
            .map(|tool| ToolDefinition {
                r#type: "function".to_string(),
                function: FunctionDefinition {
                    name: tool.name().to_string(),
                    description: tool.description().to_string(),
                    parameters: tool.parameters(),
                },
            })
            .collect();
        schemas.sort_by(|a, b| a.function.name.cmp(&b.function.name));
        schemas
    }

    /// List all registered tool names (static + dynamic).
    pub fn tool_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .tools
            .keys()
            .chain(self.dynamic.read().unwrap().keys())
            .cloned()
            .collect();
        names.sort();
        names
    }

    /// Check if a tool is registered (static or dynamic).
    pub fn has(&self, name: &str) -> bool {
        self.tools.contains_key(name) || self.dynamic.read().unwrap().contains_key(name)
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

    struct EchoTool;

    #[async_trait]
    impl Tool for EchoTool {
        fn name(&self) -> &str {
            "echo_tool"
        }
        fn description(&self) -> &str {
            "echoes args"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }
        async fn execute(&self, args: &str, _ctx: &ToolContext) -> Result<String> {
            Ok(args.to_string())
        }
    }

    fn test_ctx() -> ToolContext {
        ToolContext {
            user_id: "u".to_string(),
            workspace_root: "/tmp".to_string(),
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

    #[tokio::test]
    async fn test_dynamic_registration_lifecycle() {
        let registry = Arc::new(ToolRegistry::new());

        // Not registered yet
        assert!(!registry.has("echo_tool"));
        assert!(registry.get("echo_tool").is_none());

        // Register dynamically through a shared reference
        registry.register_dynamic(Arc::new(EchoTool));
        assert!(registry.has("echo_tool"));
        assert!(registry.tool_names().contains(&"echo_tool".to_string()));
        assert!(registry
            .all_schemas()
            .iter()
            .any(|t| t.function.name == "echo_tool"));

        // Dispatch works for dynamic tools
        let out = registry.dispatch("echo_tool", "{\"a\":1}", &test_ctx()).await.unwrap();
        assert_eq!(out, "{\"a\":1}");

        // Isolated dispatch also works
        let out = registry
            .dispatch_isolated("echo_tool", "hello", &test_ctx())
            .await
            .unwrap();
        assert_eq!(out, "hello");

        // Unregister removes it everywhere
        registry.unregister_dynamic("echo_tool");
        assert!(!registry.has("echo_tool"));
        assert!(registry.get("echo_tool").is_none());
        let err = registry.dispatch("echo_tool", "{}", &test_ctx()).await.unwrap_err();
        assert!(err.to_string().contains("Unknown tool"));

        // Unregistering an unknown name is a no-op
        registry.unregister_dynamic("echo_tool");
    }

    #[tokio::test]
    async fn test_static_tools_take_precedence_over_dynamic() {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(EchoTool));
        let registry = Arc::new(registry);

        // A dynamic registration under the same name must not shadow the
        // static tool: get() returns the static instance first.
        struct ShadowTool;
        #[async_trait]
        impl Tool for ShadowTool {
            fn name(&self) -> &str {
                "echo_tool"
            }
            fn description(&self) -> &str {
                "shadow"
            }
            fn parameters(&self) -> serde_json::Value {
                serde_json::json!({"type": "object"})
            }
            async fn execute(&self, _args: &str, _ctx: &ToolContext) -> Result<String> {
                Ok("shadow".to_string())
            }
        }
        registry.register_dynamic(Arc::new(ShadowTool));
        let out = registry.dispatch("echo_tool", "hi", &test_ctx()).await.unwrap();
        assert_eq!(out, "hi", "static tool must win over dynamic shadow");
    }
}
