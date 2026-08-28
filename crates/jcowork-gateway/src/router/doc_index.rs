//! Document index endpoints: FTS search, vector search, chunks, images,
//! Excel preview, re-indexing, and Docling service management.

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::Deserialize;

use super::{AppState, AuthUser};
use jcowork_storage::{FileStore, WorkspaceIndex};

// --- Workspace Index Search API ---

#[derive(Debug, Deserialize)]
pub(crate) struct SearchIndexQuery {
    q: String,
    #[serde(default = "default_search_limit")]
    limit: u32,
}

fn default_search_limit() -> u32 {
    10
}

pub(crate) async fn search_workspace_index(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Query(query): Query<SearchIndexQuery>,
) -> impl IntoResponse {
    let index = match WorkspaceIndex::cached(&state.data_dir, &auth_user.user_id).await {
        Ok(idx) => idx,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("Failed to open index: {}", e) })),
            ).into_response();
        }
    };

    match index.search(&query.q, query.limit).await {
        Ok(docs) => {
            (StatusCode::OK, Json(serde_json::json!({ "results": docs, "total": docs.len() }))).into_response()
        }
        Err(e) => {
            // FTS query syntax error - return empty results with error message
            (
                StatusCode::OK,
                Json(serde_json::json!({ "results": [], "total": 0, "error": format!("Search error: {}", e) })),
            ).into_response()
        }
    }
}

// --- Workspace Index List API ---

#[derive(Debug, Deserialize)]
pub(crate) struct ListIndexQuery {
    dir: Option<String>,
}

pub(crate) async fn list_workspace_index(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Query(query): Query<ListIndexQuery>,
) -> impl IntoResponse {
    let index = match WorkspaceIndex::cached(&state.data_dir, &auth_user.user_id).await {
        Ok(idx) => idx,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("Failed to open index: {}", e) })),
            ).into_response();
        }
    };

    let result = if let Some(ref dir) = query.dir {
        index.list_by_directory(dir).await
    } else {
        index.list_all(None).await
    };

    match result {
        Ok(docs) => {
            (StatusCode::OK, Json(serde_json::json!({ "documents": docs, "total": docs.len() }))).into_response()
        }
        Err(e) => {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("Failed to list: {}", e) })),
            ).into_response()
        }
    }
}

// --- Get Indexed Document Content API ---

#[derive(Debug, Deserialize)]
pub(crate) struct ContentIndexQuery {
    path: String,
    /// 0-based character offset for paginated preview (requires `limit`).
    offset: Option<i64>,
    /// Max characters to return per page.
    limit: Option<i64>,
}

pub(crate) async fn get_indexed_content(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Query(query): Query<ContentIndexQuery>,
) -> impl IntoResponse {
    let index = match WorkspaceIndex::cached(&state.data_dir, &auth_user.user_id).await {
        Ok(idx) => idx,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("Failed to open index: {}", e) })),
            ).into_response();
        }
    };

    // Paginated mode: return a character slice plus metadata for "load more".
    if let Some(limit) = query.limit {
        let offset = query.offset.unwrap_or(0).max(0);
        let limit = limit.clamp(1, 200_000);
        return match index.get_content_slice(&query.path, offset, limit).await {
            Ok(Some((content, total_len))) => {
                let next_offset = offset + content.chars().count() as i64;
                (StatusCode::OK, Json(serde_json::json!({
                    "path": query.path,
                    "content": content,
                    "total_len": total_len,
                    "next_offset": next_offset,
                    "has_more": next_offset < total_len,
                }))).into_response()
            }
            Ok(None) => {
                (StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": "Document not indexed" }))).into_response()
            }
            Err(e) => {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({ "error": format!("Failed to get content: {}", e) })),
                ).into_response()
            }
        };
    }

    match index.get_content(&query.path).await {
        Ok(Some(content)) => {
            (StatusCode::OK, Json(serde_json::json!({ "path": query.path, "content": content }))).into_response()
        }
        Ok(None) => {
            (StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": "Document not indexed" }))).into_response()
        }
        Err(e) => {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("Failed to get content: {}", e) })),
            ).into_response()
        }
    }
}

// --- Vector Search API ---

#[derive(Debug, Deserialize)]
pub(crate) struct VectorSearchQuery {
    query: String,
    #[serde(default = "default_top_k")]
    top_k: u32,
    file_path: Option<String>,
}

fn default_top_k() -> u32 {
    5
}

/// Semantic search over document chunks using vector embeddings.
/// Returns relevant sections ranked by similarity score.
pub(crate) async fn vector_search_chunks(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Query(query): Query<VectorSearchQuery>,
) -> impl IntoResponse {
    let index = match WorkspaceIndex::cached(&state.data_dir, &auth_user.user_id).await {
        Ok(idx) => idx,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("Failed to open index: {}", e) })),
            ).into_response();
        }
    };

    // Perform hybrid search (vector + FTS fallback)
    let file_paths = query.file_path.as_ref().map(|p| vec![p.clone()]);
    match index.hybrid_search(&query.query, query.top_k, file_paths.as_deref()).await {
        Ok(chunks) => {
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "query": query.query,
                    "results": chunks,
                    "count": chunks.len(),
                })),
            ).into_response()
        }
        Err(e) => {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("Search failed: {}", e) })),
            ).into_response()
        }
    }
}

// --- Document Chunks API ---

#[derive(Debug, Deserialize)]
pub(crate) struct DocChunksQuery {
    file_path: String,
}

/// Get all indexed chunks for a specific document file.
pub(crate) async fn get_document_chunks(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Query(query): Query<DocChunksQuery>,
) -> impl IntoResponse {
    let index = match WorkspaceIndex::cached(&state.data_dir, &auth_user.user_id).await {
        Ok(idx) => idx,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("Failed to open index: {}", e) })),
            ).into_response();
        }
    };

    match index.get_file_chunks(&query.file_path).await {
        Ok(chunks) => {
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "file_path": query.file_path,
                    "chunks": chunks,
                    "count": chunks.len(),
                })),
            ).into_response()
        }
        Err(e) => {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("Failed to get chunks: {}", e) })),
            ).into_response()
        }
    }
}

// --- Document Image Proxy API ---

#[derive(Debug, Deserialize)]
pub(crate) struct DocImageQuery {
    file_path: String,
    filename: String,
}

/// Proxy image assets from the Docling service.
///
/// Looks up the document's `doc_hash` from the workspace index, then proxies
/// the image from the Docling service's `/assets/{doc_hash}/{filename}` endpoint.
/// This allows the frontend to display PDF images without direct access to the Docling service.
pub(crate) async fn get_document_image(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Query(query): Query<DocImageQuery>,
) -> impl IntoResponse {
    // Validate filename to prevent path traversal
    if query.filename.contains("..") || query.filename.contains('/') || query.filename.contains('\\') {
        return (
            StatusCode::BAD_REQUEST,
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            serde_json::to_vec(&serde_json::json!({ "error": "Invalid filename" })).unwrap_or_default(),
        ).into_response();
    }

    // Look up doc_hash from workspace index
    let index = match WorkspaceIndex::cached(&state.data_dir, &auth_user.user_id).await {
        Ok(idx) => idx,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                serde_json::to_vec(&serde_json::json!({ "error": format!("Failed to open index: {}", e) })).unwrap_or_default(),
            ).into_response();
        }
    };

    let doc_hash = match index.get_doc_hash(&query.file_path).await {
        Ok(Some(hash)) => hash,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                serde_json::to_vec(&serde_json::json!({ "error": "Document not indexed or no images" })).unwrap_or_default(),
            ).into_response();
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                serde_json::to_vec(&serde_json::json!({ "error": format!("Failed to get doc hash: {}", e) })).unwrap_or_default(),
            ).into_response();
        }
    };

    // Proxy to Docling service
    let service_url = std::env::var("DOCLING_SERVICE_URL")
        .unwrap_or_else(|_| "http://localhost:50060".to_string());
    let image_url = format!("{}/assets/{}/{}", service_url, doc_hash, query.filename);

    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                serde_json::to_vec(&serde_json::json!({ "error": format!("Failed to create HTTP client: {}", e) })).unwrap_or_default(),
            ).into_response();
        }
    };

    match client.get(&image_url).send().await {
        Ok(resp) if resp.status().is_success() => {
            // Get content type from Docling response
            let content_type = resp.headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("image/png")
                .to_string();

            match resp.bytes().await {
                Ok(bytes) => (
                    StatusCode::OK,
                    [
                        (axum::http::header::CONTENT_TYPE, content_type),
                        (axum::http::header::CACHE_CONTROL, "public, max-age=86400".to_string()),
                    ],
                    bytes.to_vec(),
                ).into_response(),
                Err(e) => (
                    StatusCode::BAD_GATEWAY,
                    [(axum::http::header::CONTENT_TYPE, "application/json")],
                    serde_json::to_vec(&serde_json::json!({ "error": format!("Failed to read image: {}", e) })).unwrap_or_default(),
                ).into_response(),
            }
        }
        Ok(resp) => {
            (
                StatusCode::NOT_FOUND,
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                serde_json::to_vec(&serde_json::json!({ "error": format!("Image not found in Docling service (HTTP {})", resp.status()) })).unwrap_or_default(),
            ).into_response()
        }
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            serde_json::to_vec(&serde_json::json!({ "error": format!("Failed to fetch image from Docling: {}", e) })).unwrap_or_default(),
        ).into_response(),
    }
}

// --- Excel Database Preview API ---

/// Max rows per table returned by the excel-db preview endpoint.
const EXCEL_PREVIEW_ROWS: usize = 100;

#[derive(Debug, Deserialize)]
pub(crate) struct ExcelDbQuery {
    path: String,
}

/// Preview the SQLite database parsed from an uploaded Excel file.
/// Returns all tables with their schema and the first rows of each table.
/// If the database does not exist yet (e.g. file uploaded before Excel import
/// existed), it is built on demand from the source file.
pub(crate) async fn get_excel_db_content(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Query(query): Query<ExcelDbQuery>,
) -> impl IntoResponse {
    let ext = query.path.rsplit('.').next().unwrap_or("").to_lowercase();
    if !matches!(ext.as_str(), "xlsx" | "xls") {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "Not an Excel file" })),
        ).into_response();
    }

    let workspace_root = format!("{}/{}/workspace", state.data_dir, auth_user.user_id);
    let store = FileStore::new(&workspace_root);
    if let Err(e) = store.validate_path_public(&query.path) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": format!("Invalid path: {}", e) })),
        ).into_response();
    }

    let db_path = jcowork_storage::excel_db::db_path_for(&state.data_dir, &auth_user.user_id, &query.path);
    let src = std::path::Path::new(&workspace_root).join(&query.path);
    if !src.exists() {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "File not found in workspace" })),
        ).into_response();
    }

    // Rebuild the database when missing OR when the source Excel file is newer
    let needs_import = if !db_path.exists() {
        true
    } else {
        // Compare modification times: rebuild if source is newer than db
        let src_modified = std::fs::metadata(&src).and_then(|m| m.modified()).ok();
        let db_modified = std::fs::metadata(&db_path).and_then(|m| m.modified()).ok();
        match (src_modified, db_modified) {
            (Some(s), Some(d)) => s > d,
            _ => true, // If we can't determine times, rebuild to be safe
        }
    };

    if needs_import {
        if let Err(e) = jcowork_storage::excel_db::import_excel(
            &state.data_dir,
            &auth_user.user_id,
            &query.path,
            &workspace_root,
        ).await {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("Failed to parse Excel file: {}", e) })),
            ).into_response();
        }
    }

    match jcowork_storage::excel_db::preview_database(&db_path, EXCEL_PREVIEW_ROWS).await {
        Ok(tables) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "path": query.path,
                "db_name": jcowork_storage::excel_db::db_name_for(&query.path),
                "preview_rows": EXCEL_PREVIEW_ROWS,
                "tables": tables,
            })),
        ).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("Failed to read Excel database: {}", e) })),
        ).into_response(),
    }
}

// --- Workspace Index Re-index API ---
#[derive(Debug, Deserialize)]
pub(crate) struct ReindexRequest {
    /// Directory path to re-index (relative to workspace root). Defaults to "." (entire workspace).
    path: Option<String>,
}

pub(crate) async fn reindex_workspace_dir(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Json(body): Json<ReindexRequest>,
) -> impl IntoResponse {
    let workspace_root = format!("{}/{}/workspace", state.data_dir, auth_user.user_id);
    let store = FileStore::new(&workspace_root);
    let dir_path = body.path.as_deref().unwrap_or(".");

    // Validate path
    if let Err(e) = store.validate_path_public(dir_path) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": format!("Invalid path: {}", e) })),
        ).into_response();
    }

    // List all files recursively
    let files = match store.list_dir_recursive(dir_path).await {
        Ok(f) => f,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("Failed to list files: {}", e) })),
            ).into_response();
        }
    };

    let index = match WorkspaceIndex::cached(&state.data_dir, &auth_user.user_id).await {
        Ok(idx) => idx,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("Failed to open index: {}", e) })),
            ).into_response();
        }
    };

    let mut indexed = 0;
    let mut errors = 0;
    for file in &files {
        // Compute the full relative path
        let rel_path = if dir_path == "." {
            file.clone()
        } else {
            format!("{}/{}", dir_path, file)
        };

        match index.add_document(&rel_path, &workspace_root).await {
            Ok(()) => indexed += 1,
            Err(e) => {
                tracing::warn!(file = %rel_path, err = %e, "Failed to index file during re-index");
                errors += 1;
            }
        }
    }

    (StatusCode::OK, Json(serde_json::json!({
        "indexed": indexed,
        "errors": errors,
        "total_files": files.len(),
    }))).into_response()
}

// --- Docling Service Management API ---

/// Get the current status of the Docling service.
pub(crate) async fn get_docling_status() -> impl IntoResponse {
    let manager = jcowork_storage::DoclingManager::global();
    let status = manager.status().await;
    (StatusCode::OK, Json(serde_json::json!({
        "running": status.running,
        "starting": status.starting,
        "service_url": status.service_url,
        "message": status.message,
    }))).into_response()
}

/// Start the Docling service in the background (returns immediately).
/// The frontend should poll `/api/docling/status` to track progress.
pub(crate) async fn start_docling_service() -> impl IntoResponse {
    let manager = jcowork_storage::DoclingManager::global();

    // Already running?
    if manager.is_healthy().await {
        return (StatusCode::OK, Json(serde_json::json!({
            "ok": true,
            "message": "Docling service is already running",
        }))).into_response();
    }

    // Try to start in background.
    match manager.start_background().await {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({
            "ok": true,
            "message": "Docling service is starting, poll /api/docling/status for progress",
        }))).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "ok": false,
                "message": format!("Failed to start Docling service: {}", e),
            })),
        ).into_response(),
    }
}
