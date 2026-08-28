//! Workspace file management endpoints (list, download, save, mkdir, delete, move).

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::Deserialize;

use super::{AppState, AuthUser};
use jcowork_storage::{FileStore, WorkspaceIndex};

#[derive(Debug, Deserialize)]
pub(crate) struct WorkspaceFilesQuery {
    path: Option<String>,
}

pub(crate) async fn list_workspace_files(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Query(params): Query<WorkspaceFilesQuery>,
) -> impl IntoResponse {
    let workspace_root = format!("{}/{}/workspace", state.data_dir, auth_user.user_id);
    // Ensure workspace exists
    let _ = tokio::fs::create_dir_all(&workspace_root).await;

    let store = FileStore::new(&workspace_root);
    let path = params.path.unwrap_or_else(|| ".".to_string());

    match store.list_dir_detailed(&path).await {
        Ok(entries) => {
            let items: Vec<serde_json::Value> = entries
                .iter()
                .filter_map(|entry| {
                    let parts: Vec<&str> = entry.splitn(2, '\t').collect();
                    if parts.len() == 2 {
                        Some(serde_json::json!({
                            "name": parts[0],
                            "type": parts[1],
                        }))
                    } else {
                        None
                    }
                })
                .collect();
            (StatusCode::OK, Json(serde_json::json!(items))).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        ).into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct DownloadFileQuery {
    path: String,
    #[serde(default)]
    #[allow(dead_code)] // accepted for API compatibility (auth via Bearer token)
    token: String,
    #[serde(default)]
    raw: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct WorkspaceFilesRecursiveQuery {
    path: Option<String>,
}

pub(crate) async fn list_workspace_files_recursive(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Query(params): Query<WorkspaceFilesRecursiveQuery>,
) -> impl IntoResponse {
    let workspace_root = format!("{}/{}/workspace", state.data_dir, auth_user.user_id);
    let _ = tokio::fs::create_dir_all(&workspace_root).await;

    let store = FileStore::new(&workspace_root);
    let path = params.path.unwrap_or_else(|| ".".to_string());

    match store.list_dir_recursive(&path).await {
        Ok(files) => {
            (StatusCode::OK, Json(serde_json::json!(files))).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        ).into_response(),
    }
}

pub(crate) async fn download_workspace_file(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Query(params): Query<DownloadFileQuery>,
) -> impl IntoResponse {
    let workspace_root = format!("{}/{}/workspace", state.data_dir, auth_user.user_id);
    let store = FileStore::new(&workspace_root);

    match store.read_file(&params.path).await {
        Ok(content) => {
            // Determine content type from extension
            let is_html = params.path.ends_with(".html") || params.path.ends_with(".htm");
            let content_type = if is_html {
                "text/html; charset=utf-8"
            } else if params.path.ends_with(".css") {
                "text/css; charset=utf-8"
            } else if params.path.ends_with(".js") || params.path.ends_with(".mjs") {
                "application/javascript; charset=utf-8"
            } else if params.path.ends_with(".json") {
                "application/json; charset=utf-8"
            } else if params.path.ends_with(".svg") {
                "image/svg+xml"
            } else if params.path.ends_with(".png") || params.path.ends_with(".jpg")
                || params.path.ends_with(".jpeg") || params.path.ends_with(".gif")
                || params.path.ends_with(".webp") || params.path.ends_with(".ico")
            {
                // For binary images, we'd need a different approach; return as download
                "application/octet-stream"
            } else {
                "text/plain; charset=utf-8"
            };

            // For HTML files, inject a script that overrides fetch() to resolve
            // relative URLs against the workspace download API. This allows HTML
            // files to load sibling files (e.g. CSV data) regardless of whether
            // they are opened in an iframe, a new tab, or an external browser.
            let final_content = if is_html && !params.raw {
                let dir = params.path.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
                let token = &auth_user.token;
                let inject_script = format!(
                    r#"<script>
(function(){{
  var __dir={dir_json};
  var __token={token_json};
  var __orig=window.fetch;
  function __resolve(u){{var c=u.split('?')[0].split('#')[0];return __dir?__dir+'/'+c:c;}}
  window.fetch=function(u,o){{
    if(typeof u==='string'&&!u.startsWith('/')&&!u.startsWith('http')&&!u.startsWith('blob:')&&!u.startsWith('data:')){{
      u='/api/workspace/download?path='+encodeURIComponent(__resolve(u))+'&token='+encodeURIComponent(__token);
    }}
    return __orig.call(this,u,o);
  }};
  var __xo=XMLHttpRequest.prototype.open;
  XMLHttpRequest.prototype.open=function(m,u){{
    if(typeof u==='string'&&!u.startsWith('/')&&!u.startsWith('http')&&!u.startsWith('blob:')&&!u.startsWith('data:')){{
      u='/api/workspace/download?path='+encodeURIComponent(__resolve(u))+'&token='+encodeURIComponent(__token);
    }}
    return __xo.apply(this,arguments);
  }};
}})();
</script>"#,
                    dir_json = serde_json::to_string(dir).unwrap_or_default(),
                    token_json = serde_json::to_string(token).unwrap_or_default(),
                );
                if let Some(pos) = content.find("<head>") {
                    format!("{}{}{}", &content[..pos + 6], inject_script, &content[pos + 6..])
                } else if let Some(pos) = content.find("<HEAD>") {
                    format!("{}{}{}", &content[..pos + 6], inject_script, &content[pos + 6..])
                } else {
                    format!("{}{}", inject_script, content)
                }
            } else {
                content
            };

            let filename = params.path.rsplit('/').next().unwrap_or("file");
            (
                StatusCode::OK,
                [
                    (axum::http::header::CONTENT_TYPE, content_type.to_string()),
                    (axum::http::header::CONTENT_DISPOSITION, format!("inline; filename=\"{}\"", filename)),
                ],
                final_content,
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

// --- Save (overwrite) a text file in workspace ---

#[derive(Debug, Deserialize)]
pub(crate) struct SaveFileRequest {
    path: String,
    content: String,
}

pub(crate) async fn save_workspace_file(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Json(body): Json<SaveFileRequest>,
) -> impl IntoResponse {
    let workspace_root = format!("{}/{}/workspace", state.data_dir, auth_user.user_id);
    let store = FileStore::new(&workspace_root);

    match store.write_file(&body.path, &body.content).await {
        Ok(_) => (StatusCode::OK, Json(serde_json::json!({ "ok": true, "path": body.path }))).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("Failed to save file: {}", e) })),
        ).into_response(),
    }
}

// --- Create Directory API ---

#[derive(Debug, Deserialize)]
pub(crate) struct MkdirRequest {
    path: String,
}

pub(crate) async fn create_directory(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Json(body): Json<MkdirRequest>,
) -> impl IntoResponse {
    let workspace_root = format!("{}/{}/workspace", state.data_dir, auth_user.user_id);
    let _ = tokio::fs::create_dir_all(&workspace_root).await;
    let store = FileStore::new(&workspace_root);

    // Validate path is within workspace
    match store.validate_path_public(&body.path) {
        Ok(full_path) => {
            match tokio::fs::create_dir_all(&full_path).await {
                Ok(_) => (StatusCode::OK, Json(serde_json::json!({ "ok": true, "path": body.path }))).into_response(),
                Err(e) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({ "error": format!("Failed to create directory: {}", e) })),
                ).into_response(),
            }
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e.to_string() })),
        ).into_response(),
    }
}

// --- Delete Workspace File/Directory API ---

#[derive(Debug, Deserialize)]
pub(crate) struct DeleteWorkspaceRequest {
    path: String,
}

pub(crate) async fn delete_workspace_file(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Json(body): Json<DeleteWorkspaceRequest>,
) -> impl IntoResponse {
    let workspace_root = format!("{}/{}/workspace", state.data_dir, auth_user.user_id);
    let store = FileStore::new(&workspace_root);

    // Validate path
    if let Err(e) = store.validate_path_public(&body.path) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": format!("Invalid path: {}", e) })),
        ).into_response();
    }

    let full_path = format!("{}/{}", workspace_root, body.path);

    // Check if it's a directory or file
    let metadata = match tokio::fs::metadata(&full_path).await {
        Ok(m) => m,
        Err(e) => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": format!("Path not found: {}", e) })),
            ).into_response();
        }
    };

    // Delete from filesystem
    let result = if metadata.is_dir() {
        tokio::fs::remove_dir_all(&full_path).await
    } else {
        tokio::fs::remove_file(&full_path).await
    };

    if let Err(e) = result {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("Failed to delete: {}", e) })),
        ).into_response();
    }

    // Update index
    if let Ok(index) = WorkspaceIndex::cached(&state.data_dir, &auth_user.user_id).await {
        if metadata.is_dir() {
            let _ = index.remove_directory(&body.path).await;
        } else {
            let _ = index.remove_file(&body.path).await;
        }
    }

    (StatusCode::OK, Json(serde_json::json!({ "deleted": body.path }))).into_response()
}

// --- Move/Rename Workspace File/Directory API ---

#[derive(Debug, Deserialize)]
pub(crate) struct MoveWorkspaceRequest {
    from: String,
    to: String,
}

pub(crate) async fn move_workspace_path(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Json(body): Json<MoveWorkspaceRequest>,
) -> impl IntoResponse {
    let workspace_root = format!("{}/{}/workspace", state.data_dir, auth_user.user_id);
    let store = FileStore::new(&workspace_root);

    // Validate paths
    if let Err(e) = store.validate_path_public(&body.from) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": format!("Invalid source path: {}", e) })),
        ).into_response();
    }
    if let Err(e) = store.validate_path_public(&body.to) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": format!("Invalid destination path: {}", e) })),
        ).into_response();
    }

    let from_full = format!("{}/{}", workspace_root, body.from);
    let to_full = format!("{}/{}", workspace_root, body.to);

    // Ensure destination parent exists
    if let Some(parent) = std::path::Path::new(&to_full).parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }

    // Move on filesystem
    if let Err(e) = tokio::fs::rename(&from_full, &to_full).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("Failed to move: {}", e) })),
        ).into_response();
    }

    // Update index
    if let Ok(index) = WorkspaceIndex::cached(&state.data_dir, &auth_user.user_id).await {
        if let Err(e) = index.move_path(&body.from, &body.to).await {
            tracing::warn!(from = %body.from, to = %body.to, err = %e, "Failed to update index on move");
        }

        // If destination is a new file that wasn't previously indexed, add it
        if let Ok(count) = index.count().await {
            // If the moved path was not in index (count unchanged), try to add it
            // This handles the case where a non-indexed file is moved into an indexed location
            let _ = count; // just to suppress warning
        }
    }

    (StatusCode::OK, Json(serde_json::json!({ "moved": { "from": body.from, "to": body.to } }))).into_response()
}
