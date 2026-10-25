//! Read-only cross-user endpoints for public accounts.
//!
//! Every handler first verifies the target user exists AND has
//! `is_public == true` (404 otherwise). All data access is read-only and
//! restricted to the target's public documents and periodic-task results;
//! there is intentionally no way to add, modify, or delete another user's
//! data through these endpoints.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;

use super::{AppState, AuthUser};
use jcowork_storage::{FileStore, WorkspaceIndex};

/// Verify that `user_id` refers to an existing public account.
///
/// Returns Ok(()) when the target exists, is public, and is not in the
/// recycle bin; otherwise an already-shaped error response (404 for
/// missing/private/trashed users).
pub(crate) async fn ensure_public_user(state: &AppState, user_id: &str) -> Result<(), Response> {
    match state.user_store.get_user_by_id(user_id).await {
        Ok(Some(u)) if u.is_public && u.deleted_at.is_none() => Ok(()),
        Ok(_) => Err((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "Public user not found" })),
        )
            .into_response()),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response()),
    }
}

/// GET /api/public-users — list public accounts excluding the requester.
///
/// Returns a DTO that never includes the password hash.
pub(crate) async fn list_public_users(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
) -> impl IntoResponse {
    match state.user_store.list_public_users().await {
        Ok(users) => {
            let items: Vec<serde_json::Value> = users
                .into_iter()
                .filter(|u| u.id != auth_user.user_id)
                .map(|u| {
                    serde_json::json!({
                        "user_id": u.id,
                        "username": u.username,
                    })
                })
                .collect();
            (StatusCode::OK, Json(items)).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// GET /api/public-users/{user_id}/documents — the target's public documents.
pub(crate) async fn list_public_documents(
    State(state): State<AppState>,
    Path(user_id): Path<String>,
) -> impl IntoResponse {
    if let Err(resp) = ensure_public_user(&state, &user_id).await {
        return resp;
    }

    let index = match WorkspaceIndex::cached(&state.data_dir, &user_id).await {
        Ok(idx) => idx,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("Failed to open index: {}", e) })),
            )
                .into_response();
        }
    };

    match index.list_public(None).await {
        Ok(docs) => (
            StatusCode::OK,
            Json(serde_json::json!({ "documents": docs, "total": docs.len() })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("Failed to list: {}", e) })),
        )
            .into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct PublicContentQuery {
    path: String,
    /// 0-based character offset for paginated preview (requires `limit`).
    offset: Option<i64>,
    /// Max characters to return per page.
    limit: Option<i64>,
}

/// GET /api/public-users/{user_id}/documents/content — paginated public
/// document content. Returns 404 for documents that are not public even if
/// the target user is public (double gate).
pub(crate) async fn get_public_document_content(
    State(state): State<AppState>,
    Path(user_id): Path<String>,
    Query(query): Query<PublicContentQuery>,
) -> impl IntoResponse {
    if let Err(resp) = ensure_public_user(&state, &user_id).await {
        return resp;
    }

    let index = match WorkspaceIndex::cached(&state.data_dir, &user_id).await {
        Ok(idx) => idx,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("Failed to open index: {}", e) })),
            )
                .into_response();
        }
    };

    // Paginated mode: return a character slice plus metadata for "load more".
    if let Some(limit) = query.limit {
        let offset = query.offset.unwrap_or(0).max(0);
        let limit = limit.clamp(1, 200_000);
        return match index.get_content_slice_public(&query.path, offset, limit).await {
            Ok(Some((content, total_len))) => {
                let next_offset = offset + content.chars().count() as i64;
                (
                    StatusCode::OK,
                    Json(serde_json::json!({
                        "path": query.path,
                        "content": content,
                        "total_len": total_len,
                        "next_offset": next_offset,
                        "has_more": next_offset < total_len,
                    })),
                )
                    .into_response()
            }
            Ok(None) => (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": "Document not public or not indexed" })),
            )
                .into_response(),
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("Failed to get content: {}", e) })),
            )
                .into_response(),
        };
    }

    match index.get_content_public(&query.path).await {
        Ok(Some(content)) => (
            StatusCode::OK,
            Json(serde_json::json!({ "path": query.path, "content": content })),
        )
            .into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "Document not public or not indexed" })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("Failed to get content: {}", e) })),
        )
            .into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct PublicDownloadQuery {
    path: String,
}

/// GET /api/public-users/{user_id}/documents/download — the raw file behind a
/// public document (e.g. the original PDF for the viewer's "Original PDF"
/// mode). Double-gated like the content endpoint: the target account must be
/// public AND the document must be flagged public.
pub(crate) async fn download_public_document(
    State(state): State<AppState>,
    Path(user_id): Path<String>,
    Query(query): Query<PublicDownloadQuery>,
) -> impl IntoResponse {
    if let Err(resp) = ensure_public_user(&state, &user_id).await {
        return resp;
    }

    let index = match WorkspaceIndex::cached(&state.data_dir, &user_id).await {
        Ok(idx) => idx,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("Failed to open index: {}", e) })),
            )
                .into_response();
        }
    };

    match index.is_public(&query.path).await {
        Ok(Some(true)) => {}
        _ => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": "Document not public or not indexed" })),
            )
                .into_response();
        }
    }

    let workspace_root = format!("{}/{}/workspace", state.data_dir, user_id);
    let store = FileStore::new(&workspace_root);
    match store.read_file_bytes(&query.path).await {
        Ok(content) => {
            let filename = query.path.rsplit('/').next().unwrap_or("file");
            (
                StatusCode::OK,
                [
                    (
                        axum::http::header::CONTENT_TYPE,
                        super::workspace::content_type_for_path(&query.path).to_string(),
                    ),
                    (
                        axum::http::header::CONTENT_DISPOSITION,
                        format!("inline; filename=\"{}\"", filename),
                    ),
                ],
                content,
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

/// GET /api/public-users/{user_id}/cron-jobs — the target's periodic tasks.
pub(crate) async fn list_public_cron_jobs(
    State(state): State<AppState>,
    Path(user_id): Path<String>,
) -> impl IntoResponse {
    if let Err(resp) = ensure_public_user(&state, &user_id).await {
        return resp;
    }

    let jobs = state.cron_scheduler.list_cron_jobs(&user_id).await;
    (StatusCode::OK, Json(serde_json::json!(jobs))).into_response()
}

/// GET /api/public-users/{user_id}/cron-jobs/{job_id}/results — execution
/// results of one of the target's periodic tasks. The job must belong to
/// the target user's job list.
pub(crate) async fn get_public_cron_job_results(
    State(state): State<AppState>,
    Path((user_id, job_id)): Path<(String, String)>,
) -> impl IntoResponse {
    if let Err(resp) = ensure_public_user(&state, &user_id).await {
        return resp;
    }

    let owned = state
        .cron_scheduler
        .list_cron_jobs(&user_id)
        .await
        .iter()
        .any(|j| j.id == job_id);
    if !owned {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "Cron job not found" })),
        )
            .into_response();
    }

    let results = state.cron_scheduler.list_task_results(&job_id).await;
    (StatusCode::OK, Json(serde_json::json!(results))).into_response()
}
