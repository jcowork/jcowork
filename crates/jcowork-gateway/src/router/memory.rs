//! Memory CRUD and agent identity endpoints.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::Deserialize;

use super::{AppState, AuthUser, MemoryInfo, MemorySearchQuery, UpdateMemoryRequest};

pub(crate) async fn list_memories(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
) -> impl IntoResponse {
    match state.memory_manager.recall_all(&auth_user.user_id).await {
        Ok(entries) => {
            let infos: Vec<MemoryInfo> = entries
                .into_iter()
                .map(|e| MemoryInfo {
                    id: e.id,
                    content: e.content,
                    category: e.category,
                })
                .collect();
            (StatusCode::OK, Json(infos))
        }
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, Json(Vec::<MemoryInfo>::new())),
    }
}

pub(crate) async fn search_memories(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Query(params): Query<MemorySearchQuery>,
) -> impl IntoResponse {
    let query = params.query.unwrap_or_default();
    let limit = params.limit.unwrap_or(10) as usize;
    match state.memory_manager.search(&auth_user.user_id, &query, limit).await {
        Ok(results) => {
            let infos: Vec<MemoryInfo> = results
                .into_iter()
                .map(|r| MemoryInfo {
                    id: r.id,
                    content: r.content,
                    category: r.category,
                })
                .collect();
            (StatusCode::OK, Json(infos))
        }
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, Json(Vec::<MemoryInfo>::new())),
    }
}

pub(crate) async fn update_memory(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Path(id): Path<String>,
    Json(req): Json<UpdateMemoryRequest>,
) -> impl IntoResponse {
    match state.memory_manager.update(
        &auth_user.user_id,
        &id,
        req.content.as_deref(),
        req.category.as_deref(),
    ).await {
        Ok(entry) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "id": entry.id,
                "content": entry.content,
                "category": entry.category,
                "updated_at": entry.updated_at,
            })),
        ),
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": e.to_string()})),
        ),
    }
}

pub(crate) async fn delete_memory(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.memory_manager.delete(&auth_user.user_id, &id).await {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({"status": "deleted"}))),
        Err(e) => (StatusCode::NOT_FOUND, Json(serde_json::json!({"error": e.to_string()}))),
    }
}

pub(crate) async fn get_agent_identity(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
) -> impl IntoResponse {
    match state.memory_manager.search(&auth_user.user_id, "agent_identity", 1).await {
        Ok(results) => {
            let identity = results.into_iter().next().map(|r| r.content).unwrap_or_default();
            (StatusCode::OK, Json(serde_json::json!({ "identity": identity })))
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "identity": "", "error": e.to_string() })),
        ),
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct SetAgentIdentityRequest {
    pub identity: String,
}

pub(crate) async fn set_agent_identity(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Json(req): Json<SetAgentIdentityRequest>,
) -> impl IntoResponse {
    // Delete existing agent_identity entries first
    if let Ok(entries) = state.memory_manager.recall_all(&auth_user.user_id).await {
        for entry in entries.into_iter().filter(|e| e.category == "agent_identity") {
            let _ = state.memory_manager.delete(&auth_user.user_id, &entry.id).await;
        }
    }
    // Save new identity (empty string = reset to default)
    let identity = req.identity.trim().to_string();
    if identity.is_empty() {
        return (StatusCode::OK, Json(serde_json::json!({ "status": "reset" })));
    }
    match state.memory_manager.save(&auth_user.user_id, &identity, "agent_identity").await {
        Ok(_) => (StatusCode::OK, Json(serde_json::json!({ "status": "saved" }))),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        ),
    }
}
