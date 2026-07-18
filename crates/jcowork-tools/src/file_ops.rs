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
    fn description(&self) -> &str { "List files in a directory within the user's workspace (non-recursive, with type info)." }
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
        let entries = store.list_dir_detailed(path).await?;
        Ok(entries.join("\n"))
    }
}

/// File delete tool.
pub struct FileDeleteTool;

#[async_trait]
impl Tool for FileDeleteTool {
    fn name(&self) -> &str { "file_delete" }
    fn description(&self) -> &str { "Delete a file in the user's workspace." }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Relative path to the file to delete" }
            },
            "required": ["path"]
        })
    }
    async fn execute(&self, args: &str, ctx: &ToolContext) -> Result<String> {
        let parsed: serde_json::Value = serde_json::from_str(args)?;
        let path = parsed["path"].as_str().ok_or_else(|| anyhow::anyhow!("Missing 'path'"))?;
        let store = FileStore::new(&ctx.workspace_root);
        store.delete_file(path).await?;
        Ok(format!("Deleted {}", path))
    }
}

/// File move/rename tool.
pub struct FileMoveTool;

#[async_trait]
impl Tool for FileMoveTool {
    fn name(&self) -> &str { "file_move" }
    fn description(&self) -> &str { "Move or rename a file/directory to a new path within the workspace." }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "from": { "type": "string", "description": "Relative source path" },
                "to": { "type": "string", "description": "Relative destination path" }
            },
            "required": ["from", "to"]
        })
    }
    async fn execute(&self, args: &str, ctx: &ToolContext) -> Result<String> {
        let parsed: serde_json::Value = serde_json::from_str(args)?;
        let from = parsed["from"].as_str().ok_or_else(|| anyhow::anyhow!("Missing 'from'"))?;
        let to = parsed["to"].as_str().ok_or_else(|| anyhow::anyhow!("Missing 'to'"))?;
        let store = FileStore::new(&ctx.workspace_root);
        store.move_path(from, to).await?;
        Ok(format!("Moved {} to {}", from, to))
    }
}

/// File copy tool.
pub struct FileCopyTool;

#[async_trait]
impl Tool for FileCopyTool {
    fn name(&self) -> &str { "file_copy" }
    fn description(&self) -> &str { "Copy a file to a new location within the workspace." }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "from": { "type": "string", "description": "Relative source path" },
                "to": { "type": "string", "description": "Relative destination path" }
            },
            "required": ["from", "to"]
        })
    }
    async fn execute(&self, args: &str, ctx: &ToolContext) -> Result<String> {
        let parsed: serde_json::Value = serde_json::from_str(args)?;
        let from = parsed["from"].as_str().ok_or_else(|| anyhow::anyhow!("Missing 'from'"))?;
        let to = parsed["to"].as_str().ok_or_else(|| anyhow::anyhow!("Missing 'to'"))?;
        let store = FileStore::new(&ctx.workspace_root);
        store.copy_file(from, to).await?;
        Ok(format!("Copied {} to {}", from, to))
    }
}

/// File search (grep) tool.
pub struct FileSearchTool;

#[async_trait]
impl Tool for FileSearchTool {
    fn name(&self) -> &str { "file_search" }
    fn description(&self) -> &str { "Search file contents for a pattern (substring match) recursively within a directory. Returns matching lines with file path and line number." }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Search pattern (substring to match)" },
                "path": { "type": "string", "description": "Relative directory to search in (default: .)", "default": "." }
            },
            "required": ["pattern"]
        })
    }
    async fn execute(&self, args: &str, ctx: &ToolContext) -> Result<String> {
        let parsed: serde_json::Value = serde_json::from_str(args)?;
        let pattern = parsed["pattern"].as_str().ok_or_else(|| anyhow::anyhow!("Missing 'pattern'"))?;
        let path = parsed["path"].as_str().unwrap_or(".");
        let store = FileStore::new(&ctx.workspace_root);
        let results = store.search_content(pattern, path).await?;
        if results.is_empty() {
            Ok("No matches found.".to_string())
        } else {
            Ok(results.join("\n"))
        }
    }
}

/// Directory create tool.
pub struct DirCreateTool;

#[async_trait]
impl Tool for DirCreateTool {
    fn name(&self) -> &str { "dir_create" }
    fn description(&self) -> &str { "Create a directory (and all parent directories) within the user's workspace." }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Relative directory path to create" }
            },
            "required": ["path"]
        })
    }
    async fn execute(&self, args: &str, ctx: &ToolContext) -> Result<String> {
        let parsed: serde_json::Value = serde_json::from_str(args)?;
        let path = parsed["path"].as_str().ok_or_else(|| anyhow::anyhow!("Missing 'path'"))?;
        let store = FileStore::new(&ctx.workspace_root);
        store.create_dir(path).await?;
        Ok(format!("Created directory {}", path))
    }
}

/// Directory recursive list tool.
pub struct DirListTool;

#[async_trait]
impl Tool for DirListTool {
    fn name(&self) -> &str { "dir_list" }
    fn description(&self) -> &str { "Recursively list all files under a directory (skips .git, node_modules, target, __pycache__). Useful for understanding project structure." }
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
        let entries = store.list_dir_recursive(path).await?;
        if entries.is_empty() {
            Ok("(empty or not found)".to_string())
        } else {
            Ok(entries.join("\n"))
        }
    }
}

/// File info tool.
pub struct FileInfoTool;

#[async_trait]
impl Tool for FileInfoTool {
    fn name(&self) -> &str { "file_info" }
    fn description(&self) -> &str { "Get file metadata: type (file/directory), size, and last modified time." }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Relative path to the file or directory" }
            },
            "required": ["path"]
        })
    }
    async fn execute(&self, args: &str, ctx: &ToolContext) -> Result<String> {
        let parsed: serde_json::Value = serde_json::from_str(args)?;
        let path = parsed["path"].as_str().ok_or_else(|| anyhow::anyhow!("Missing 'path'"))?;
        let store = FileStore::new(&ctx.workspace_root);
        store.file_info(path).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_ctx(dir: &std::path::Path) -> ToolContext {
        ToolContext {
            user_id: "test-user".to_string(),
            workspace_root: dir.to_string_lossy().to_string(),
        }
    }

    #[tokio::test]
    async fn test_file_write_and_read_tool() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = make_ctx(dir.path());

        // Write a file via the tool
        let args = r#"{"path":"hello.html","content":"<h1>Hi</h1>"}"#;
        let result = FileWriteTool.execute(args, &ctx).await.unwrap();
        assert!(result.contains("Written"));

        // Read it back via the tool
        let args = r#"{"path":"hello.html"}"#;
        let content = FileReadTool.execute(args, &ctx).await.unwrap();
        assert_eq!(content, "<h1>Hi</h1>");
    }

    #[tokio::test]
    async fn test_file_list_tool() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = make_ctx(dir.path());

        FileWriteTool.execute(r#"{"path":"a.txt","content":"1"}"#, &ctx).await.unwrap();
        FileWriteTool.execute(r#"{"path":"b.txt","content":"2"}"#, &ctx).await.unwrap();

        let result = FileListTool.execute(r#"{"path":"."}"#, &ctx).await.unwrap();
        assert!(result.contains("a.txt\tfile"));
        assert!(result.contains("b.txt\tfile"));
    }

    #[tokio::test]
    async fn test_file_delete_tool() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = make_ctx(dir.path());

        FileWriteTool.execute(r#"{"path":"temp.txt","content":"x"}"#, &ctx).await.unwrap();
        let result = FileDeleteTool.execute(r#"{"path":"temp.txt"}"#, &ctx).await.unwrap();
        assert!(result.contains("Deleted"));

        // Verify it's gone
        assert!(FileReadTool.execute(r#"{"path":"temp.txt"}"#, &ctx).await.is_err());
    }

    #[tokio::test]
    async fn test_file_move_tool() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = make_ctx(dir.path());

        FileWriteTool.execute(r#"{"path":"old.txt","content":"data"}"#, &ctx).await.unwrap();
        let result = FileMoveTool.execute(
            r#"{"from":"old.txt","to":"new.txt"}"#,
            &ctx,
        ).await.unwrap();
        assert!(result.contains("Moved"));
        assert!(FileReadTool.execute(r#"{"path":"old.txt"}"#, &ctx).await.is_err());
        let content = FileReadTool.execute(r#"{"path":"new.txt"}"#, &ctx).await.unwrap();
        assert_eq!(content, "data");
    }

    #[tokio::test]
    async fn test_file_copy_tool() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = make_ctx(dir.path());

        FileWriteTool.execute(r#"{"path":"orig.txt","content":"copy me"}"#, &ctx).await.unwrap();
        FileCopyTool.execute(
            r#"{"from":"orig.txt","to":"dup.txt"}"#,
            &ctx,
        ).await.unwrap();

        // Both should exist
        assert_eq!(FileReadTool.execute(r#"{"path":"orig.txt"}"#, &ctx).await.unwrap(), "copy me");
        assert_eq!(FileReadTool.execute(r#"{"path":"dup.txt"}"#, &ctx).await.unwrap(), "copy me");
    }

    #[tokio::test]
    async fn test_dir_create_and_list_tool() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = make_ctx(dir.path());

        DirCreateTool.execute(r#"{"path":"project/src"}"#, &ctx).await.unwrap();
        FileWriteTool.execute(r#"{"path":"project/src/main.js","content":"console.log(1)"}"#, &ctx).await.unwrap();
        FileWriteTool.execute(r#"{"path":"project/index.html","content":"<html></html>"}"#, &ctx).await.unwrap();

        let result = DirListTool.execute(r#"{"path":"project"}"#, &ctx).await.unwrap();
        assert!(result.contains("index.html"));
        assert!(result.contains("src/main.js"));
    }

    #[tokio::test]
    async fn test_file_search_tool() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = make_ctx(dir.path());

        FileWriteTool.execute(r#"{"path":"app.py","content":"def foo():\n    return 'bar'"}"#, &ctx).await.unwrap();
        FileWriteTool.execute("{\"path\":\"other.py\",\"content\":\"nothing here\"}", &ctx).await.unwrap();

        let result = FileSearchTool.execute(
            r#"{"pattern":"foo","path":"."}"#,
            &ctx,
        ).await.unwrap();
        assert!(result.contains("app.py"));
        assert!(result.contains("foo"));
    }

    #[tokio::test]
    async fn test_file_info_tool() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = make_ctx(dir.path());

        FileWriteTool.execute(r#"{"path":"test.txt","content":"hello"}"#, &ctx).await.unwrap();
        let result = FileInfoTool.execute(r#"{"path":"test.txt"}"#, &ctx).await.unwrap();
        assert!(result.contains("type: file"));
        assert!(result.contains("size: 5 bytes"));
    }

    #[tokio::test]
    async fn test_html_iteration_via_tools() {
        // End-to-end: create HTML, read it, modify it, verify each iteration
        let dir = tempfile::tempdir().unwrap();
        let ctx = make_ctx(dir.path());

        // v1: minimal page
        FileWriteTool.execute(
            r#"{"path":"page.html","content":"<h1>v1</h1>"}"#,
            &ctx,
        ).await.unwrap();
        assert_eq!(
            FileReadTool.execute(r#"{"path":"page.html"}"#, &ctx).await.unwrap(),
            "<h1>v1</h1>"
        );

        // v2: add styling
        let html_v2 = "{\"path\":\"page.html\",\"content\":\"<style>body{color:red}</style><h1>v2</h1>\"}";
        FileWriteTool.execute(html_v2, &ctx).await.unwrap();
        let read_v2 = FileReadTool.execute(r#"{"path":"page.html"}"#, &ctx).await.unwrap();
        assert!(read_v2.contains("<style>"));
        assert!(read_v2.contains("v2"));

        // v3: add script
        let html_v3 = "{\"path\":\"page.html\",\"content\":\"<script>console.log(42)</script><h1>v3</h1>\"}";
        FileWriteTool.execute(html_v3, &ctx).await.unwrap();
        let read_v3 = FileReadTool.execute(r#"{"path":"page.html"}"#, &ctx).await.unwrap();
        assert!(read_v3.contains("<script>"));
        assert!(read_v3.contains("v3"));

        // Search for 'console' across workspace
        let search = FileSearchTool.execute(r#"{"pattern":"console","path":"."}"#, &ctx).await.unwrap();
        assert!(search.contains("page.html"));
    }
}
