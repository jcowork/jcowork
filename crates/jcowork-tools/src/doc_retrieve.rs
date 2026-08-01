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
        "Semantically search uploaded documents by meaning using vector embeddings. This is the PRIMARY search tool — prefer this over doc_search for most queries, especially Chinese text. Returns relevant document sections ranked by semantic similarity. Use this when the user asks about content in their documents. NOTE: it returns only the most relevant FRAGMENTS, each with its character Offset in the document — when fragments are not enough (e.g. the user asks for the full/complete text 全文), call doc_content with the fragment's file_path and Offset to keep reading forward from that position, and stop once you have enough to answer. QUERY RULES: keep the query short and use the user's own words as-is (e.g. user asks 雨的四季全文 → query: 雨的四季). Do NOT add extra terms, synonyms, author names, or filler words like 全文/内容/课文 — they dilute semantic matching and hurt recall. If no results, try doc_list to see available documents. Do not call more than twice with different queries."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Short search keywords copied from the user's original wording. Do not add words the user did not say (no author names, no 全文/内容/课文, no paraphrasing)."
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

        // Locate each fragment's position within its document so the caller
        // can continue reading the full text from there via doc_content.
        let mut full_texts: std::collections::HashMap<String, Option<String>> = std::collections::HashMap::new();
        for chunk in &results {
            if !full_texts.contains_key(&chunk.file_path) {
                let full = index.get_content(&chunk.file_path).await.unwrap_or(None);
                full_texts.insert(chunk.file_path.clone(), full);
            }
        }

        let mut output = format!("Found {} relevant section(s):\n\n", results.len());
        let mut any_offset = false;
        
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

            let offset = full_texts
                .get(&chunk.file_path)
                .and_then(|f| f.as_deref())
                .and_then(|full| jcowork_storage::locate_offset(full, &chunk.content));
            if let Some(off) = offset {
                any_offset = true;
                output.push_str(&format!("   Offset: {}\n", off));
            }
            
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

        if any_offset {
            output.push_str(
                "Tip: these are fragments only. If they are not enough to answer (e.g. the user wants the full text or more context), call doc_content with the section's File and Offset to keep reading forward from that position. Stop reading once you have enough."
            );
        }

        Ok(output.trim_end().to_string())
    }
}

/// Full document content tool — reads the complete indexed text of a document, page by page.
pub struct DocContentTool;

#[derive(Deserialize)]
struct DocContentArgs {
    file_path: String,
    /// 0-based character offset to start reading from (default: 0)
    #[serde(default)]
    offset: i64,
    /// Max characters to return in one page (default: 20000)
    #[serde(default = "default_content_limit")]
    limit: i64,
}

fn default_content_limit() -> i64 {
    20_000
}

#[async_trait]
impl Tool for DocContentTool {
    fn name(&self) -> &str {
        "doc_content"
    }

    fn description(&self) -> &str {
        "Read the full indexed text content of a document (PDF/markdown) by file path, one page at a time. Use this when the user asks for the full text / complete content (全文) of a document — doc_retrieve only returns relevant fragments. Use doc_list to find the file path if unknown. When the output says more content is available, call again with the given offset to continue reading."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "The file path of the document (as shown by doc_list)"
                },
                "offset": {
                    "type": "integer",
                    "description": "0-based character offset to read from (an Offset returned by doc_retrieve, or the next_offset from a previous doc_content call)",
                    "default": 0
                },
                "limit": {
                    "type": "integer",
                    "description": "Max characters to return in this page (default: 20000)",
                    "default": 20000
                }
            },
            "required": ["file_path"]
        })
    }

    async fn execute(&self, args: &str, ctx: &ToolContext) -> Result<String> {
        let parsed: DocContentArgs = serde_json::from_str(args)?;

        let workspace_path = std::path::Path::new(&ctx.workspace_root);
        let data_dir = workspace_path
            .parent()
            .and_then(|p| p.parent())
            .map(|p| p.to_string_lossy().to_string())
            .ok_or_else(|| anyhow::anyhow!("Cannot determine data_dir from workspace_root"))?;

        let index = jcowork_storage::WorkspaceIndex::cached(&data_dir, &ctx.user_id).await?;

        let offset = parsed.offset.max(0);
        let limit = parsed.limit.clamp(1, 50_000);

        match index.get_content_slice(&parsed.file_path, offset, limit).await? {
            Some((content, total_len)) => {
                let read = content.chars().count() as i64;
                let next_offset = offset + read;
                let mut output = format!(
                    "Document '{}' — characters {}..{} of {}:\n\n{}",
                    parsed.file_path, offset, next_offset, total_len, content
                );
                if next_offset < total_len {
                    output.push_str(&format!(
                        "\n\n[... {} more characters — call doc_content again with offset={} to continue ...]",
                        total_len - next_offset, next_offset
                    ));
                }
                Ok(output)
            }
            None => Ok(format!(
                "Document '{}' is not indexed. Use doc_list to see available documents.",
                parsed.file_path
            )),
        }
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

    #[tokio::test]
    async fn test_doc_content_paging() {
        let dir = tempdir().unwrap();
        let ctx = make_ctx(dir.path());

        tokio::fs::create_dir_all(&ctx.workspace_root).await.unwrap();
        tokio::fs::write(
            std::path::Path::new(&ctx.workspace_root).join("poem.md"),
            "# 雨的四季\n我喜欢雨，无论什么季节的雨，我都喜欢。",
        )
        .await
        .unwrap();

        let data_dir = dir.path().join("data_dir");
        let index = jcowork_storage::WorkspaceIndex::cached(
            &data_dir.to_string_lossy(),
            &ctx.user_id,
        )
        .await
        .unwrap();
        index.add_document("poem.md", &ctx.workspace_root).await.unwrap();

        // First page with a tiny limit returns a slice plus a continuation hint
        let page1 = DocContentTool
            .execute(r#"{"file_path":"poem.md","limit":10}"#, &ctx)
            .await
            .unwrap();
        assert!(page1.contains("characters 0.."));
        assert!(page1.contains("offset="));

        // Following the returned offset reads through to the end
        let page2 = DocContentTool
            .execute(r#"{"file_path":"poem.md","offset":10,"limit":1000}"#, &ctx)
            .await
            .unwrap();
        assert!(page2.contains("我都喜欢"));
        assert!(!page2.contains("more characters"));

        // Unknown file
        let missing = DocContentTool
            .execute(r#"{"file_path":"nope.md"}"#, &ctx)
            .await
            .unwrap();
        assert!(missing.contains("not indexed"));
    }
}
