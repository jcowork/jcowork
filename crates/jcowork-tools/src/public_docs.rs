//! Public document tools — cross-user retrieval of @mentioned public accounts'
//! public documents.
//!
//! Both tools require a `username` that must match one of the public accounts
//! @mentioned in the current conversation (provided via
//! [`ToolContext::mentioned_public_users`]). They only ever read documents
//! flagged `is_public = 1` in the target user's workspace index; private
//! documents are invisible even when the account itself is public.

use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;

use crate::base::{Tool, ToolContext, truncate_str};

/// Resolve `username` against the mentioned public users of this conversation.
///
/// Returns `(user_id, username)` on success; on failure returns a
/// user-facing explanatory message (not an error) so the model learns the
/// mention requirement instead of retrying blindly.
fn resolve_mentioned_public_user(
    ctx: &ToolContext,
    username: &str,
) -> Result<(String, String), String> {
    if let Some((id, name)) = ctx
        .mentioned_public_users
        .iter()
        .find(|(_, name)| name == username)
    {
        return Ok((id.clone(), name.clone()));
    }

    let mut msg = format!(
        "Public user '{}' was not @mentioned in this conversation, so their documents are not accessible. ",
        username
    );
    if ctx.mentioned_public_users.is_empty() {
        msg.push_str(
            "No public users are currently in scope. The user must @mention a public account (e.g. @alice) in their message before you can search that account's public documents.",
        );
    } else {
        let names: Vec<&str> = ctx
            .mentioned_public_users
            .iter()
            .map(|(_, n)| n.as_str())
            .collect();
        msg.push_str(&format!(
            "Public users mentioned in this conversation: {}.",
            names.join(", ")
        ));
    }
    Err(msg)
}

/// Compute the shared data dir from the per-user workspace root
/// (`{data_dir}/{user_id}/workspace`), same derivation as doc_search/doc_retrieve.
fn data_dir_from_ctx(ctx: &ToolContext) -> Result<String> {
    let workspace_path = std::path::Path::new(&ctx.workspace_root);
    workspace_path
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.to_string_lossy().to_string())
        .ok_or_else(|| anyhow::anyhow!("Cannot determine data_dir from workspace_root"))
}

/// Semantic search over a @mentioned public account's PUBLIC documents.
pub struct PublicDocSearchTool;

#[derive(Deserialize)]
struct PublicDocSearchArgs {
    /// Username of the @mentioned public account (without the @).
    username: String,
    query: String,
    #[serde(default = "default_top_k")]
    top_k: u32,
}

fn default_top_k() -> u32 {
    5
}

#[async_trait]
impl Tool for PublicDocSearchTool {
    fn name(&self) -> &str {
        "public_doc_search"
    }

    fn description(&self) -> &str {
        "Search the PUBLIC documents of a public account that was @mentioned in this conversation. Use this when the user asks about the content of a public user's documents (e.g. '@alice 你公开的文章讲了什么'). Only documents marked public by their owner are searchable; private documents are never returned. The `username` must exactly match a public account mentioned in the conversation — the context message lists them. Returns relevant sections ranked by similarity with their character Offset, which can be passed to public_doc_content to keep reading. Keep the query short, using the user's own words."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "username": {
                    "type": "string",
                    "description": "Username (without @) of the @mentioned public account whose public documents to search"
                },
                "query": {
                    "type": "string",
                    "description": "Short search keywords copied from the user's original wording"
                },
                "top_k": {
                    "type": "integer",
                    "description": "Maximum number of relevant sections to return (default: 5)",
                    "default": 5
                }
            },
            "required": ["username", "query"]
        })
    }

    async fn execute(&self, args: &str, ctx: &ToolContext) -> Result<String> {
        let parsed: PublicDocSearchArgs = serde_json::from_str(args)?;

        let (target_user_id, target_username) =
            match resolve_mentioned_public_user(ctx, &parsed.username) {
                Ok(t) => t,
                Err(msg) => return Ok(msg),
            };

        tracing::info!(
            target_user_id = %target_user_id,
            target_username = %target_username,
            query = %parsed.query,
            top_k = parsed.top_k,
            "public_doc_search called"
        );

        let data_dir = data_dir_from_ctx(ctx)?;
        let index = jcowork_storage::WorkspaceIndex::cached(&data_dir, &target_user_id).await?;

        // Public-only chunk search (vector, with public FTS fallback).
        let results = index
            .search_chunks_public(&parsed.query, parsed.top_k)
            .await?;

        if results.is_empty() {
            // No matching fragments — list the target's public documents instead.
            let public_docs = index.list_public(None).await.unwrap_or_default();
            if public_docs.is_empty() {
                return Ok(format!(
                    "@{} has no public documents (or none are indexed yet).",
                    target_username
                ));
            }
            let mut output = format!(
                "No relevant content found for \"{}\" in @{}'s public documents (tried semantic and keyword search).\n\n{} public document(s) are available:\n",
                parsed.query,
                target_username,
                public_docs.len()
            );
            for doc in &public_docs {
                output.push_str(&format!("- {} ({})\n", doc.filename, doc.content_type));
            }
            output.push_str("\nTry rephrasing your query with different terms.");
            return Ok(output);
        }

        // Locate each fragment's position within its (public) document so the
        // caller can continue reading via public_doc_content.
        let mut full_texts: std::collections::HashMap<String, Option<String>> =
            std::collections::HashMap::new();
        for chunk in &results {
            if !full_texts.contains_key(&chunk.file_path) {
                let full = index
                    .get_content_public(&chunk.file_path)
                    .await
                    .unwrap_or(None);
                full_texts.insert(chunk.file_path.clone(), full);
            }
        }

        let mut output = format!(
            "Found {} relevant section(s) in @{}'s PUBLIC documents:\n\n",
            results.len(),
            target_username
        );
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

            let content_preview = if chunk.content.len() > 500 {
                format!("{}...", truncate_str(&chunk.content, 500))
            } else {
                chunk.content.clone()
            };
            output.push_str(&format!("   Content: {}\n", content_preview));

            output.push('\n');
        }

        if any_offset {
            output.push_str(&format!(
                "Tip: these are fragments only. If they are not enough, call public_doc_content with username=\"{}\", the section's File and Offset to keep reading forward from that position.",
                target_username
            ));
        }

        Ok(output.trim_end().to_string())
    }
}

/// Full-text reader for a @mentioned public account's PUBLIC documents.
pub struct PublicDocContentTool;

#[derive(Deserialize)]
struct PublicDocContentArgs {
    /// Username of the @mentioned public account (without the @).
    username: String,
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
impl Tool for PublicDocContentTool {
    fn name(&self) -> &str {
        "public_doc_content"
    }

    fn description(&self) -> &str {
        "Read the full indexed text of one of a @mentioned public account's PUBLIC documents, one page at a time. Use this after public_doc_search when fragments are not enough (e.g. the user asks for the full text). Only documents marked public by their owner are readable; the username must exactly match a public account mentioned in the conversation, and file_path must be one of their public documents (see public_doc_search). When the output says more content is available, call again with the given offset to continue reading."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "username": {
                    "type": "string",
                    "description": "Username (without @) of the @mentioned public account whose public document to read"
                },
                "file_path": {
                    "type": "string",
                    "description": "File path of the public document (as shown by public_doc_search)"
                },
                "offset": {
                    "type": "integer",
                    "description": "0-based character offset to read from (an Offset returned by public_doc_search, or next_offset from a previous public_doc_content call)",
                    "default": 0
                },
                "limit": {
                    "type": "integer",
                    "description": "Max characters to return in this page (default: 20000)",
                    "default": 20000
                }
            },
            "required": ["username", "file_path"]
        })
    }

    async fn execute(&self, args: &str, ctx: &ToolContext) -> Result<String> {
        let parsed: PublicDocContentArgs = serde_json::from_str(args)?;

        let (target_user_id, target_username) =
            match resolve_mentioned_public_user(ctx, &parsed.username) {
                Ok(t) => t,
                Err(msg) => return Ok(msg),
            };

        tracing::info!(
            target_user_id = %target_user_id,
            target_username = %target_username,
            file_path = %parsed.file_path,
            "public_doc_content called"
        );

        let data_dir = data_dir_from_ctx(ctx)?;
        let index = jcowork_storage::WorkspaceIndex::cached(&data_dir, &target_user_id).await?;

        let offset = parsed.offset.max(0);
        let limit = parsed.limit.clamp(1, 50_000);

        match index
            .get_content_slice_public(&parsed.file_path, offset, limit)
            .await?
        {
            Some((content, total_len)) => {
                let read = content.chars().count() as i64;
                let next_offset = offset + read;
                let mut output = format!(
                    "Public document '{}' (by @{}) — characters {}..{} of {}:\n\n{}",
                    parsed.file_path, target_username, offset, next_offset, total_len, content
                );
                if next_offset < total_len {
                    output.push_str(&format!(
                        "\n\n[... {} more characters — call public_doc_content again with username=\"{}\", offset={} to continue ...]",
                        total_len - next_offset,
                        target_username,
                        next_offset
                    ));
                }
                Ok(output)
            }
            None => Ok(format!(
                "Public document '{}' of @{} is not available (it is not public or not indexed). Use public_doc_search to see their public documents.",
                parsed.file_path, target_username
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    async fn setup(dir: &std::path::Path) -> ToolContext {
        // Layout: dir/data_dir/{owner}/workspace
        let owner_workspace = dir
            .join("data_dir")
            .join("owner-user")
            .join("workspace");
        tokio::fs::create_dir_all(&owner_workspace).await.unwrap();
        tokio::fs::write(owner_workspace.join("public.md"), "# Public\nsharing public knowledge")
            .await
            .unwrap();
        tokio::fs::write(owner_workspace.join("private.md"), "# Private\nsharing private knowledge")
            .await
            .unwrap();

        let data_dir = dir.join("data_dir");
        let index = jcowork_storage::WorkspaceIndex::cached(
            &data_dir.to_string_lossy(),
            "owner-user",
        )
        .await
        .unwrap();
        let ws = owner_workspace.to_string_lossy().to_string();
        index.add_document("public.md", &ws).await.unwrap();
        index.add_document("private.md", &ws).await.unwrap();
        index.set_public("public.md", true).await.unwrap();

        // The *viewer* context (a different user) that mentioned @alice
        ToolContext {
            user_id: "viewer-user".to_string(),
            workspace_root: dir
                .join("data_dir")
                .join("viewer-user")
                .join("workspace")
                .to_string_lossy()
                .to_string(),
            mentioned_public_users: vec![("owner-user".to_string(), "alice".to_string())],
        }
    }

    #[tokio::test]
    async fn test_public_tools_require_mention() {
        let dir = tempdir().unwrap();
        let mut ctx = setup(dir.path()).await;

        // No mentions in scope: tools must refuse with an explanatory message.
        ctx.mentioned_public_users.clear();
        let res = PublicDocSearchTool
            .execute(r#"{"username":"alice","query":"knowledge"}"#, &ctx)
            .await
            .unwrap();
        assert!(res.contains("was not @mentioned"), "unexpected: {}", res);

        let res = PublicDocContentTool
            .execute(r#"{"username":"alice","file_path":"public.md"}"#, &ctx)
            .await
            .unwrap();
        assert!(res.contains("was not @mentioned"), "unexpected: {}", res);

        // Unknown username with a non-empty mention list also refuses.
        let res = PublicDocSearchTool
            .execute(r#"{"username":"bob","query":"knowledge"}"#, &ctx)
            .await
            .unwrap();
        assert!(res.contains("was not @mentioned"), "unexpected: {}", res);
    }

    #[tokio::test]
    async fn test_public_doc_content_gates_on_public_flag() {
        let dir = tempdir().unwrap();
        let ctx = setup(dir.path()).await;

        // Public document is readable
        let res = PublicDocContentTool
            .execute(r#"{"username":"alice","file_path":"public.md","limit":100}"#, &ctx)
            .await
            .unwrap();
        assert!(res.contains("Public document 'public.md'"), "unexpected: {}", res);
        assert!(res.contains("sharing public knowledge"));

        // Private document of the same account is NOT readable
        let res = PublicDocContentTool
            .execute(r#"{"username":"alice","file_path":"private.md","limit":100}"#, &ctx)
            .await
            .unwrap();
        assert!(res.contains("not available"), "unexpected: {}", res);
        assert!(!res.contains("sharing private knowledge"));
    }

    #[tokio::test]
    async fn test_public_doc_search_never_returns_private() {
        let dir = tempdir().unwrap();
        let ctx = setup(dir.path()).await;

        // "sharing" matches both documents; results must only ever include
        // the public one (or legitimately report no matches — the embedding
        // service may be unavailable in tests, in which case the public FTS
        // fallback still works on manually inserted chunks).
        let res = PublicDocSearchTool
            .execute(r#"{"username":"alice","query":"sharing"}"#, &ctx)
            .await
            .unwrap();
        assert!(!res.contains("was not @mentioned"), "unexpected: {}", res);
        assert!(!res.contains("private.md"), "private doc leaked: {}", res);
    }
}
