//! File operations tool - read, write, list, search files in user workspace.

use anyhow::Result;
use async_trait::async_trait;
use jcowork_storage::file_store::FileStore;

use crate::base::{Tool, ToolContext};

/// File read tool.
pub struct FileReadTool;

#[async_trait]
impl Tool for FileReadTool {
    fn name(&self) -> &str { "file_read" }
    fn description(&self) -> &str { "Read the contents of a file in the user's workspace." }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Relative path to the file within workspace" }
            },
            "required": ["path"]
        })
    }
    async fn execute(&self, args: &str, ctx: &ToolContext) -> Result<String> {
        let parsed: serde_json::Value = serde_json::from_str(args)?;
        let path = parsed["path"].as_str().ok_or_else(|| anyhow::anyhow!("Missing 'path'"))?;
        let store = FileStore::new(&ctx.workspace_root);
        store.read_file(path).await
    }
}

/// File write tool.
pub struct FileWriteTool;

#[async_trait]
impl Tool for FileWriteTool {
    fn name(&self) -> &str { "file_write" }
    fn description(&self) -> &str { "Write content to a file in the user's workspace." }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Relative path to the file within workspace" },
                "content": { "type": "string", "description": "Content to write" }
            },
            "required": ["path", "content"]
        })
    }
    async fn execute(&self, args: &str, ctx: &ToolContext) -> Result<String> {
        let parsed: serde_json::Value = serde_json::from_str(args)?;
        let path = parsed["path"].as_str().ok_or_else(|| anyhow::anyhow!("Missing 'path'"))?;
        let content = parsed["content"].as_str().ok_or_else(|| anyhow::anyhow!("Missing 'content'"))?;
        let store = FileStore::new(&ctx.workspace_root);
        store.write_file(path, content).await?;
        Ok(format!("Written to {}", path))
    }
}

/// File list tool.
pub struct FileListTool;

#[async_trait]
impl Tool for FileListTool {
    fn name(&self) -> &str { "file_list" }
    fn description(&self) -> &str { "List files in a directory within the user's workspace." }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Relative directory path (default: .)", "default": "." }
            }
        })
    }
    async fn execute(&self, args: &str, ctx: &ToolContext) -> Result<String> {
        let parsed: serde_json::Value = serde_json::from_str(args).unwrap_or_default();
        let path = parsed["path"].as_str().unwrap_or(".");
        let store = FileStore::new(&ctx.workspace_root);
        let entries = store.list_dir(path).await?;
        Ok(entries.join("\n"))
    }
}
