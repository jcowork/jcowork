//! Tool trait definition.

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// A tool's JSON schema parameter definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolParameter {
    pub name: String,
    #[serde(rename = "type")]
    pub param_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r#enum: Option<Vec<String>>,
}

/// Context provided to tool execution (user-scoped).
#[derive(Debug, Clone)]
pub struct ToolContext {
    pub user_id: String,
    pub workspace_root: String,
}

/// Trait that all tools must implement.
///
/// Each tool provides:
/// - A name and description for LLM discovery
/// - A JSON schema for parameter validation
/// - An async execute method that takes arguments and returns a string result
#[async_trait]
pub trait Tool: Send + Sync {
    /// Unique tool name (e.g., "shell", "file_read").
    fn name(&self) -> &str;

    /// Human-readable description shown to the LLM.
    fn description(&self) -> &str;

    /// JSON Schema for the tool's parameters.
    fn parameters(&self) -> serde_json::Value;

    /// Execute the tool with the given arguments and context.
    async fn execute(&self, args: &str, ctx: &ToolContext) -> Result<String>;
}
