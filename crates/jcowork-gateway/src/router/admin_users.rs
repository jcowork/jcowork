//! Admin (super-user) user management endpoints.
//!
//! Only accounts flagged with `is_admin = 1` (the seeded `admin` account)
//! may call these endpoints. The admin can search users, move them to the
//! recycle bin, restore them, and permanently delete them.
//!
//! Trashed accounts: permanently purged `TRASH_RETENTION_DAYS` days after
//! deletion (background purger), cannot log in, and are invisible to other
//! users (public listing + public profile endpoints exclude them).
//!
//! Permanent deletion removes the account record only — the per-user data
//! directory is intentionally left on disk.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;

use super::{AppState, AuthUser};
use jcowork_storage::user_store::TRASH_RETENTION_DAYS;
use jcowork_storage::user_store::User;

/// Query parameters for the admin user list.
#[derive(Debug, Deserialize)]
pub(crate) struct AdminUserQuery {
    /// Username substring filter (case-insensitive).
    pub query: Option<String>,
    /// "active" (default) | "trash" | "all"
    pub status: Option<String>,
}

/// Ensure the caller is a super user. The middleware already resolves
/// `is_admin` from the DB on every request, so a simple flag check suffices.
fn ensure_admin(auth_user: &AuthUser) -> Result<(), Response> {
    if auth_user.is_admin {
        return Ok(());
    }
    Err((
        StatusCode::FORBIDDEN,
        Json(serde_json::json!({ "error": "Admin access required" })),
    )
        .into_response())
}

/// Map a user row to a safe DTO (never exposes `password_hash`).
fn user_dto(u: &User) -> serde_json::Value {
    serde_json::json!({
        "user_id": u.id,
        "username": u.username,
        "is_public": u.is_public,
        "is_admin": u.is_admin,
        "created_at": u.created_at,
        "deleted_at": u.deleted_at,
        "days_left": u.deleted_at.as_deref().map(days_left_in_trash),
    })
}

/// Days remaining before a trashed account is purged permanently (0 = due).
fn days_left_in_trash(deleted_at: &str) -> i64 {
    match chrono::NaiveDateTime::parse_from_str(deleted_at, "%Y-%m-%d %H:%M:%S") {
        Ok(dt) => {
            let elapsed = chrono::Utc::now()
                .naive_utc()
                .signed_duration_since(dt)
                .num_days();
            (TRASH_RETENTION_DAYS - elapsed).max(0)
        }
        Err(_) => 0,
    }
}

/// GET /api/admin/users?query=&status=active|trash|all — list/search users.
pub(crate) async fn list_users(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Query(query): Query<AdminUserQuery>,
) -> impl IntoResponse {
    if let Err(resp) = ensure_admin(&auth_user) {
        return resp;
    }

    let status = query.status.as_deref().unwrap_or("active");
    match state.user_store.list_users(query.query.as_deref(), status).await {
        Ok(users) => {
            let items: Vec<serde_json::Value> = users.iter().map(user_dto).collect();
            (StatusCode::OK, Json(items)).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// Load the target user and reject admin accounts (which must never be
/// trashed or permanently deleted).
async fn load_deletable_target(
    state: &AppState,
    auth_user: &AuthUser,
    user_id: &str,
) -> Result<User, Response> {
    match state.user_store.get_user_by_id(user_id).await {
        Ok(Some(target)) => {
            if target.is_admin {
                return Err((
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({ "error": "Cannot delete admin account" })),
                )
                    .into_response());
            }
            if target.id == auth_user.user_id {
                return Err((
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({ "error": "Cannot delete your own account" })),
                )
                    .into_response());
            }
            Ok(target)
        }
        Ok(None) => Err((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "User not found" })),
        )
            .into_response()),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response()),
    }
}

/// POST /api/admin/users/{id}/trash — move a user to the recycle bin.
pub(crate) async fn trash_user(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Path(user_id): Path<String>,
) -> impl IntoResponse {
    if let Err(resp) = ensure_admin(&auth_user) {
        return resp;
    }

    let target = match load_deletable_target(&state, &auth_user, &user_id).await {
        Ok(t) => t,
        Err(resp) => return resp,
    };
    if target.deleted_at.is_some() {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "error": "User is already in the trash" })),
        )
            .into_response();
    }

    match state.user_store.soft_delete_user(&user_id).await {
        Ok(()) => {
            tracing::info!(user_id = %user_id, username = %target.username, "Admin moved user to trash");
            (
                StatusCode::OK,
                Json(serde_json::json!({ "message": "User moved to trash" })),
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// POST /api/admin/users/{id}/restore — restore a user from the recycle bin.
pub(crate) async fn restore_user(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Path(user_id): Path<String>,
) -> impl IntoResponse {
    if let Err(resp) = ensure_admin(&auth_user) {
        return resp;
    }

    match state.user_store.get_user_by_id(&user_id).await {
        Ok(Some(target)) if target.deleted_at.is_some() => {
            match state.user_store.restore_user(&user_id).await {
                Ok(()) => {
                    tracing::info!(user_id = %user_id, username = %target.username, "Admin restored user from trash");
                    (
                        StatusCode::OK,
                        Json(serde_json::json!({ "message": "User restored" })),
                    )
                        .into_response()
                }
                Err(e) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({ "error": e.to_string() })),
                )
                    .into_response(),
            }
        }
        Ok(Some(_)) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "error": "User is not in the trash" })),
        )
            .into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "User not found" })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// DELETE /api/admin/users/{id} — permanently delete the account record.
///
/// Only the `users` table row is removed; the per-user data directory stays
/// on disk.
pub(crate) async fn permanently_delete_user(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Path(user_id): Path<String>,
) -> impl IntoResponse {
    if let Err(resp) = ensure_admin(&auth_user) {
        return resp;
    }

    let target = match load_deletable_target(&state, &auth_user, &user_id).await {
        Ok(t) => t,
        Err(resp) => return resp,
    };

    match state.user_store.permanently_delete_user(&user_id).await {
        Ok(()) => {
            tracing::info!(user_id = %user_id, username = %target.username, "Admin permanently deleted user account");
            (
                StatusCode::OK,
                Json(serde_json::json!({ "message": "User permanently deleted" })),
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}
