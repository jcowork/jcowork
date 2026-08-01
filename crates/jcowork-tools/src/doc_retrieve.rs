//! doc_retrieve tool — semantic search over document chunks.
//!
//! Uses vector embeddings to find relevant document sections based on
//! semantic similarity to the query. Falls back to FTS5 keyword search
//! when the embedding service is unavailable.

use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;

use crate::base::{Tool, ToolContext, truncate_str};

/// Document semantic retrieval tool.
pub struct DocRetrieveTool;

#[derive(Deserialize)]
struct DocRetrieveArgs {
    query: String,
    #[serde(default = "default_top_k")]
    top_k: u32,
    /// Optional: restrict search to specific file(s)
    file_path: Option<String>,
}

fn default_top_k() -> u32 {
    5
}

#[async_trait]
impl Tool for DocRetrieveTool {
    fn name(&self) -> &str {
        "doc_retrieve"
    }

    fn description(&self) -> &str {
        "Semantically search uploaded documents by meaning using vector embeddings. This is the PRIMARY search tool — prefer this over doc_search for most queries, especially Chinese text. Returns relevant document sections ranked by semantic similarity. Use this when the user asks about content in their documents. If no results, try doc_list to see available documents. Do not call more than twice with different queries."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Natural language query describing what information to find"
                },
                "top_k": {
                    "type": "integer",
                    "description": "Maximum number of relevant sections to return (default: 5)",
                    "default": 5
                },
                "file_path": {
                    "type": "string",
                    "description": "Optional: restrict search to a specific file path"
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(&self, args: &str, ctx: &ToolContext) -> Result<String> {
        let parsed: DocRetrieveArgs = serde_json::from_str(args)?;
        
        tracing::info!(query = %parsed.query, top_k = parsed.top_k, file_path = ?parsed.file_path, "doc_retrieve called");

        // Compute the data_dir from workspace_root
        let workspace_path = std::path::Path::new(&ctx.workspace_root);
        let data_dir = workspace_path
            .parent()
            .and_then(|p| p.parent())
            .map(|p| p.to_string_lossy().to_string())
            .ok_or_else(|| anyhow::anyhow!("Cannot determine data_dir from workspace_root"))?;
        
        tracing::info!(data_dir = %data_dir, user_id = %ctx.user_id, "Opening workspace index");

        let index = jcowork_storage::WorkspaceIndex::cached(&data_dir, &ctx.user_id).await?;
        
        tracing::info!("Starting hybrid search (semantic + keyword fallback)");

        // Step 1: Try semantic search (vector embeddings)
        let file_paths = parsed.file_path.as_ref().map(|p| vec![p.clone()]);
        let mut results = index
            .hybrid_search(&parsed.query, parsed.top_k, file_paths.as_deref())
            .await?;
        
        tracing::info!(result_count = results.len(), "Semantic search completed");

        // Step 2: If semantic search returns no results, fallback to FTS5 keyword search
        if results.is_empty() {
            tracing::info!("Semantic search returned no results, trying FTS5 keyword search");
            results = index
                .fts_chunk_search(&parsed.query, parsed.top_k)
                .await
                .unwrap_or_default();
            tracing::info!(result_count = results.len(), "FTS5 keyword search completed");
        }

        if results.is_empty() {
            // Both semantic and keyword search failed — list available documents
            let all_docs = index.list_all(None).await.unwrap_or_default();
            let mut output = String::new();
            
            if !all_docs.is_empty() {
                output.push_str(&format!("No relevant content found for \"{}\" (tried both semantic and keyword search).\n\n", parsed.query));
                output.push_str(&format!("{} document(s) are indexed:\n", all_docs.len()));
                for doc in &all_docs {
                    output.push_str(&format!("- {} ({})\n", doc.filename, doc.content_type));
                }
                output.push_str("\nTry rephrasing your query with different terms.");
            } else {
                output = "No documents are indexed yet. Upload documents through the Documents page.".to_string();
            }
            
            return Ok(output);
        }

        let mut output = format!("Found {} relevant section(s):\n\n", results.len());
        
        for (i, chunk) in results.iter().enumerate() {
            let type_icon = match chunk.chunk_type.as_str() {
                "table" => "📊",
                "image" => "🖼️",
                _ => "📄",
            };
            
            output.push_str(&format!(
                "{} **Section {} (score: {:.3})**\n",
                type_icon,
                i + 1,
                chunk.score
            ));
            output.push_str(&format!("   File: {}\n", chunk.file_path));
            
            if !chunk.heading.is_empty() {
                output.push_str(&format!("   Heading: {}\n", chunk.heading));
            }
            
            output.push_str(&format!("   Type: {}\n", chunk.chunk_type));
            
            // Show content (truncated if too long, at a UTF-8 char boundary)
            let content_preview = if chunk.content.len() > 500 {
                format!("{}...", truncate_str(&chunk.content, 500))
            } else {
                chunk.content.clone()
            };
            output.push_str(&format!("   Content: {}\n", content_preview));
            
            if let Some(ref img_path) = chunk.image_path {
                output.push_str(&format!("   Image path: {}\n", img_path));
            }
            
            output.push('\n');
        }

        Ok(output.trim_end().to_string())
    }
}

/// Document chunk list tool — lists all chunks for a file.
pub struct DocChunksTool;

#[derive(Deserialize)]
struct DocChunksArgs {
    file_path: String,
}

#[async_trait]
impl Tool for DocChunksTool {
    fn name(&self) -> &str {
        "doc_chunks"
    }

    fn description(&self) -> &str {
        "List all indexed chunks for a specific document file. Shows the document structure including text sections, tables, and images."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "The file path of the document to list chunks for"
                }
            },
            "required": ["file_path"]
        })
    }

    async fn execute(&self, args: &str, ctx: &ToolContext) -> Result<String> {
        let parsed: DocChunksArgs = serde_json::from_str(args)?;

        let workspace_path = std::path::Path::new(&ctx.workspace_root);
        let data_dir = workspace_path
            .parent()
            .and_then(|p| p.parent())
            .map(|p| p.to_string_lossy().to_string())
            .ok_or_else(|| anyhow::anyhow!("Cannot determine data_dir from workspace_root"))?;

        let index = jcowork_storage::WorkspaceIndex::cached(&data_dir, &ctx.user_id).await?;
        let chunks = index.get_file_chunks(&parsed.file_path).await?;

        if chunks.is_empty() {
            return Ok(format!(
                "No indexed chunks found for '{}'. The file may not be indexed yet.",
                parsed.file_path
            ));
        }

        let mut output = format!("Document '{}' has {} indexed chunk(s):\n\n", parsed.file_path, chunks.len());
        
        for chunk in &chunks {
            let type_icon = match chunk.chunk_type.as_str() {
                "table" => "📊",
                "image" => "🖼️",
                _ => "📄",
            };
            
            output.push_str(&format!(
                "{} Chunk #{} ({})\n",
                type_icon, chunk.chunk_index, chunk.chunk_type
            ));
            
            if !chunk.heading.is_empty() {
                output.push_str(&format!("   Heading: {}\n", chunk.heading));
            }
            
            let content_preview = if chunk.content.len() > 200 {
                format!("{}...", truncate_str(&chunk.content, 200))
            } else {
                chunk.content.clone()
            };
            output.push_str(&format!("   Preview: {}\n\n", content_preview));
        }

        Ok(output.trim_end().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn make_ctx(dir: &std::path::Path) -> ToolContext {
        ToolContext {
            user_id: "test-user".to_string(),
            workspace_root: dir.join("data_dir").join("test-user").join("workspace").to_string_lossy().to_string(),
        }
    }

    #[tokio::test]
    async fn test_doc_retrieve_empty() {
        let dir = tempdir().unwrap();
        let ctx = make_ctx(dir.path());

        // Create workspace dir
        tokio::fs::create_dir_all(&ctx.workspace_root).await.unwrap();

        let result = DocRetrieveTool
            .execute(r#"{"query":"test"}"#, &ctx)
            .await
            .unwrap();
        assert!(result.contains("No relevant content") || result.contains("No documents are indexed"));
    }

    #[tokio::test]
    async fn test_doc_chunks_empty() {
        let dir = tempdir().unwrap();
        let ctx = make_ctx(dir.path());

        tokio::fs::create_dir_all(&ctx.workspace_root).await.unwrap();

        let result = DocChunksTool
            .execute(r#"{"file_path":"test.pdf"}"#, &ctx)
            .await
            .unwrap();
        assert!(result.contains("No indexed chunks"));
    }
}
