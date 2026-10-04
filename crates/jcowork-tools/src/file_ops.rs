//! File operations tool - read, write, list, search files in user workspace.

use anyhow::Result;
use async_trait::async_trait;
use jcowork_storage::file_store::FileStore;

use crate::base::{Tool, ToolContext};

/// File read tool.
pub struct FileReadTool;

/// Maximum bytes returned by a single `file_read` call. Larger files must be
/// read in slices via `offset`/`limit` so a huge file (e.g. a big skill
/// document) cannot flood the model context.
const READ_MAX_BYTES: usize = 64 * 1024;

#[async_trait]
impl Tool for FileReadTool {
    fn name(&self) -> &str { "file_read" }
    fn description(&self) -> &str {
        "Read the contents of a file in the user's workspace. Large files are read in slices: pass offset (1-based start line) and limit (max lines); the footer of a truncated result tells you the next offset. Use file_search first to locate the line of interest in big files."
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Relative path to the file within workspace" },
                "offset": { "type": "integer", "description": "1-based line number to start reading from (default 1)" },
                "limit": { "type": "integer", "description": "Maximum number of lines to return (default: until the output byte cap)" }
            },
            "required": ["path"]
        })
    }
    async fn execute(&self, args: &str, ctx: &ToolContext) -> Result<String> {
        let parsed: serde_json::Value = serde_json::from_str(args)?;
        let path = parsed["path"].as_str().ok_or_else(|| anyhow::anyhow!("Missing 'path'"))?;
        let offset = parsed["offset"].as_u64().unwrap_or(1).max(1) as usize;
        let limit = parsed["limit"].as_u64().map(|l| l.max(1) as usize);

        let store = FileStore::new(&ctx.workspace_root);
        let content = store.read_file(path).await?;

        // Fast path: whole small file, no slicing requested — return verbatim.
        if offset == 1 && limit.is_none() && content.len() <= READ_MAX_BYTES {
            return Ok(content);
        }

        let lines: Vec<&str> = content.lines().collect();
        let total_lines = lines.len();
        if offset > total_lines {
            return Ok(format!(
                "[empty: offset {} is past the end of {} ({} lines total)]",
                offset, path, total_lines
            ));
        }

        let start = offset - 1; // 0-based
        let requested_end = match limit {
            Some(l) => (start + l).min(total_lines),
            None => total_lines,
        };

        // Fill the slice until the byte budget is exhausted.
        let mut bytes = 0usize;
        let mut end = start;
        for i in start..requested_end {
            let line_bytes = lines[i].len() + 1;
            if bytes + line_bytes > READ_MAX_BYTES {
                break;
            }
            bytes += line_bytes;
            end = i + 1;
        }
        // Always return at least one line, even when it alone exceeds the cap.
        if end == start && start < requested_end {
            end = start + 1;
        }

        let mut out = lines[start..end].join("\n");
        if end < total_lines {
            out.push_str(&format!(
                "\n\n[{} has {} lines; showing lines {}-{}. Continue with file_read offset={} limit={} to read more.]",
                path,
                total_lines,
                start + 1,
                end,
                end + 1,
                limit.unwrap_or(200)
            ));
        }
        Ok(out)
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

        // Update workspace index
        if let Some(data_dir) = compute_data_dir(&ctx.workspace_root) {
            if let Ok(index) = jcowork_storage::WorkspaceIndex::cached(&data_dir, &ctx.user_id).await {
                let _ = index.remove_file(path).await;
            }
        }

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

        // Update workspace index
        if let Some(data_dir) = compute_data_dir(&ctx.workspace_root) {
            if let Ok(index) = jcowork_storage::WorkspaceIndex::cached(&data_dir, &ctx.user_id).await {
                let _ = index.move_path(from, to).await;
            }
        }

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

/// Compute the data_dir from workspace_root.
/// workspace_root = {data_dir}/{user_id}/workspace
/// So data_dir = grandparent of workspace_root's parent.
fn compute_data_dir(workspace_root: &str) -> Option<String> {
    std::path::Path::new(workspace_root)
        .parent() // {data_dir}/{user_id}
        .and_then(|p| p.parent()) // {data_dir}
        .map(|p| p.to_string_lossy().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_ctx(dir: &std::path::Path) -> ToolContext {
        ToolContext {
            user_id: "test-user".to_string(),
            workspace_root: dir.to_string_lossy().to_string(),
            mentioned_public_users: Vec::new(),
            model: None,
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
    async fn test_file_read_offset_limit() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = make_ctx(dir.path());

        let body: String = (1..=500).map(|i| format!("line {}\n", i)).collect();
        FileWriteTool.execute(&serde_json::json!({"path": "big.txt", "content": body}).to_string(), &ctx)
            .await
            .unwrap();

        // offset + limit slice
        let result = FileReadTool
            .execute(r#"{"path":"big.txt","offset":100,"limit":3}"#, &ctx)
            .await
            .unwrap();
        assert!(result.starts_with("line 100\nline 101\nline 102"));
        assert!(result.contains("Continue with file_read offset=103"));

        // offset past EOF
        let result = FileReadTool
            .execute(r#"{"path":"big.txt","offset":9999}"#, &ctx)
            .await
            .unwrap();
        assert!(result.contains("past the end"));

        // Small file: unchanged behaviour (verbatim, no footer)
        FileWriteTool.execute(r#"{"path":"small.txt","content":"a\nb\n"}"#, &ctx).await.unwrap();
        let result = FileReadTool.execute(r#"{"path":"small.txt"}"#, &ctx).await.unwrap();
        assert_eq!(result, "a\nb\n");
    }

    #[tokio::test]
    async fn test_file_read_caps_large_output() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = make_ctx(dir.path());

        // ~200 KB across many lines: default read must cap at READ_MAX_BYTES
        let body: String = (1..=20000).map(|i| format!("payload line {:05}\n", i)).collect();
        FileWriteTool.execute(&serde_json::json!({"path": "huge.txt", "content": body}).to_string(), &ctx)
            .await
            .unwrap();

        let result = FileReadTool.execute(r#"{"path":"huge.txt"}"#, &ctx).await.unwrap();
        assert!(result.len() <= READ_MAX_BYTES + 512, "output should be capped, got {}", result.len());
        assert!(result.contains("[huge.txt has 20000 lines; showing lines 1-"));
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
