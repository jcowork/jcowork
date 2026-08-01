//! doc_search tool — searches the workspace document index.
//!
//! Queries the per-user workspace index (SQLite FTS5) to find documents
//! by keyword. The index is automatically populated when files are uploaded
//! through the Documents page.

use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;

use crate::base::{Tool, ToolContext};

/// Document search tool that queries the workspace index.
pub struct DocSearchTool;

#[derive(Deserialize)]
struct DocSearchArgs {
    query: String,
    #[serde(default = "default_limit")]
    limit: u32,
}

fn default_limit() -> u32 {
    10
}

#[async_trait]
impl Tool for DocSearchTool {
    fn name(&self) -> &str {
        "doc_search"
    }

    fn description(&self) -> &str {
        "Search uploaded documents by exact keyword using full-text search. Returns matching documents with file paths and snippets. NOTE: This uses keyword matching which may not work well for Chinese text. For semantic/meaning-based search, use doc_retrieve instead. Do not call this tool more than once — if no results, try doc_retrieve or doc_list."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search keyword or phrase to find in document contents"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of results to return (default: 10)",
                    "default": 10
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(&self, args: &str, ctx: &ToolContext) -> Result<String> {
        let parsed: DocSearchArgs = serde_json::from_str(args)?;

        // Compute the data_dir from workspace_root
        // workspace_root = {data_dir}/{user_id}/workspace
        // So data_dir = workspace_root's grandparent's parent
        let workspace_path = std::path::Path::new(&ctx.workspace_root);
        let data_dir = workspace_path
            .parent() // {data_dir}/{user_id}
            .and_then(|p| p.parent()) // {data_dir}
            .map(|p| p.to_string_lossy().to_string())
            .ok_or_else(|| anyhow::anyhow!("Cannot determine data_dir from workspace_root"))?;

        let index = jcowork_storage::WorkspaceIndex::cached(&data_dir, &ctx.user_id).await?;

        let results = index.search(&parsed.query, parsed.limit).await?;

        if results.is_empty() {
            // When keyword search fails, list available documents and suggest alternatives
            let all_docs = index.list_all(None).await.unwrap_or_default();
            let mut output = format!("No documents matched the keyword \"{}\".\n\n", parsed.query);
            
            if !all_docs.is_empty() {
                output.push_str(&format!("However, {} document(s) are indexed:\n", all_docs.len()));
                for doc in &all_docs {
                    output.push_str(&format!("- {} ({})\n", doc.filename, doc.content_type));
                }
                output.push_str("\nSuggestions:\n");
                output.push_str("1. Use doc_retrieve with a semantic query (e.g., \"秋雨相关的文章\") for meaning-based search.\n");
                output.push_str("2. Use doc_list to see all available documents.\n");
                output.push_str("3. Try different keywords or broader terms.\n");
            } else {
                output.push_str("No documents are indexed yet. Upload documents through the Documents page.");
            }
            
            return Ok(output);
        }

        let mut output = format!("Found {} document(s):\n\n", results.len());
        for (i, doc) in results.iter().enumerate() {
            output.push_str(&format!(
                "{}. **{}** ({})\n   Path: {}\n   Size: {} bytes\n   Indexed: {}\n",
                i + 1,
                doc.filename,
                doc.content_type,
                doc.file_path,
                doc.size,
                doc.indexed_at,
            ));
            if !doc.snippet.is_empty() {
                let snippet = if doc.snippet.len() > 300 {
                    format!("{}...", &doc.snippet[..300])
                } else {
                    doc.snippet.clone()
                };
                output.push_str(&format!("   Preview: {}\n", snippet));
            }
            output.push('\n');
        }

        Ok(output.trim_end().to_string())
    }
}

/// Document list tool — lists all indexed documents.
pub struct DocListTool;

#[derive(Deserialize)]
struct DocListArgs {
    dir: Option<String>,
}

#[async_trait]
impl Tool for DocListTool {
    fn name(&self) -> &str {
        "doc_list"
    }

    fn description(&self) -> &str {
        "List all indexed documents in the workspace. Optionally filter by directory path. Shows filename, type, size, and path for each document."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "dir": {
                    "type": "string",
                    "description": "Directory path to filter by (optional, lists all if omitted)"
                }
            }
        })
    }

    async fn execute(&self, args: &str, ctx: &ToolContext) -> Result<String> {
        let parsed: Option<DocListArgs> = serde_json::from_str(args).ok().flatten();

        let workspace_path = std::path::Path::new(&ctx.workspace_root);
        let data_dir = workspace_path
            .parent()
            .and_then(|p| p.parent())
            .map(|p| p.to_string_lossy().to_string())
            .ok_or_else(|| anyhow::anyhow!("Cannot determine data_dir from workspace_root"))?;

        let index = jcowork_storage::WorkspaceIndex::cached(&data_dir, &ctx.user_id).await?;

        let results = if let Some(ref dir) = parsed.as_ref().and_then(|p| p.dir.as_ref()) {
            index.list_by_directory(dir).await?
        } else {
            index.list_all(None).await?
        };

        if results.is_empty() {
            return Ok("No indexed documents found. Upload documents through the Documents page to index them.".to_string());
        }

        let mut output = format!("Indexed documents ({}):\n\n", results.len());
        for doc in &results {
            output.push_str(&format!(
                "- {} ({}, {} bytes) — {}\n",
                doc.filename, doc.content_type, doc.size, doc.file_path,
            ));
        }

        Ok(output.trim_end().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn make_ctx(dir: &std::path::Path) -> ToolContext {
        // Create structure: dir/data_dir/test-user/workspace
        // so compute_data_dir(workspace) = dir/data_dir
        ToolContext {
            user_id: "test-user".to_string(),
            workspace_root: dir.join("data_dir").join("test-user").join("workspace").to_string_lossy().to_string(),
        }
    }

    #[tokio::test]
    async fn test_doc_search_empty() {
        let dir = tempdir().unwrap();
        let ctx = make_ctx(dir.path());

        // Create workspace dir
        tokio::fs::create_dir_all(&ctx.workspace_root).await.unwrap();

        let result = DocSearchTool
            .execute(r#"{"query":"test"}"#, &ctx)
            .await
            .unwrap();
        assert!(result.contains("No documents matched") || result.contains("No indexed documents"));
    }

    #[tokio::test]
    async fn test_doc_list_empty() {
        let dir = tempdir().unwrap();
        let ctx = make_ctx(dir.path());

        tokio::fs::create_dir_all(&ctx.workspace_root).await.unwrap();

        let result = DocListTool
            .execute("{}", &ctx)
            .await
            .unwrap();
        assert!(result.contains("No indexed documents"));
    }
}
