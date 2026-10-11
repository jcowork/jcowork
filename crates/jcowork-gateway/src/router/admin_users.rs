//! Admin (super-user) user management endpoints.
//!
//! Only accounts flagged with `is_admin = 1` (the seeded `admin` account)
//! may call these endpoints. The admin can search users, assign or remove
//! their phone number, move them to the recycle bin, restore them, and
//! permanently delete them.
//!
//! Phone assignment matters more than the rest: SMS code is the only way a
//! normal account can authenticate, so an account without a number is
//! unreachable until an admin gives it one.
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

use super::{AppState, AuthUser, mask_email, mask_phone};
use crate::auth::normalize_phone;
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
pub(crate) fn ensure_admin(auth_user: &AuthUser) -> Result<(), Response> {
    if auth_user.is_admin {
        return Ok(());
    }
    Err((
        StatusCode::FORBIDDEN,
        Json(serde_json::json!({ "error": "Admin access required" })),
    )
        .into_response())
}

/// Map a user row to a safe DTO (never exposes `password_hash`, and only ever
/// the masked phone number / email address).
fn user_dto(u: &User) -> serde_json::Value {
    serde_json::json!({
        "user_id": u.id,
        "username": u.username,
        "phone_masked": u.phone.as_deref().map(mask_phone).unwrap_or_default(),
        "phone_bound": u.phone.is_some(),
        "email_masked": u.email.as_deref().map(mask_email).unwrap_or_default(),
        "email_bound": u.email.is_some(),
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

/// Request body for the public-flag toggle.
#[derive(Debug, Deserialize)]
pub(crate) struct SetPublicRequest {
    pub is_public: bool,
}

/// POST /api/admin/users/{id}/public — set or clear a user's public flag.
///
/// Making an account private immediately hides it (and its shared content)
/// from every other user; making it public re-publishes it.
pub(crate) async fn set_user_public(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Path(user_id): Path<String>,
    Json(req): Json<SetPublicRequest>,
) -> impl IntoResponse {
    if let Err(resp) = ensure_admin(&auth_user) {
        return resp;
    }

    // Verify the target exists (any account, including admin, may be toggled)
    let target = match state.user_store.get_user_by_id(&user_id).await {
        Ok(Some(t)) => t,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": "User not found" })),
            )
                .into_response();
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response();
        }
    };

    match state.user_store.set_user_public(&user_id, req.is_public).await {
        Ok(()) => {
            tracing::info!(
                user_id = %user_id,
                username = %target.username,
                is_public = req.is_public,
                "Admin updated user public flag"
            );
            let message = if req.is_public {
                "User is now public"
            } else {
                "User is now private"
            };
            (
                StatusCode::OK,
                Json(serde_json::json!({ "message": message, "is_public": req.is_public })),
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

/// Request body for assigning a phone number.
#[derive(Debug, Deserialize)]
pub(crate) struct SetPhoneRequest {
    pub phone: String,
}

/// POST /api/admin/users/{id}/phone — assign or reassign a phone number.
///
/// This is the recovery path for accounts created before SMS login existed:
/// they hold no number, so they cannot authenticate at all until an admin gives
/// them one. Binding also stamps the SMS-only password sentinel, leaving the
/// account in exactly the state a self-service bind produces.
pub(crate) async fn set_user_phone(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Path(user_id): Path<String>,
    Json(req): Json<SetPhoneRequest>,
) -> impl IntoResponse {
    if let Err(resp) = ensure_admin(&auth_user) {
        return resp;
    }

    let Some(phone) = normalize_phone(&req.phone) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "Invalid phone number" })),
        )
            .into_response();
    };

    let target = match state.user_store.get_user_by_id(&user_id).await {
        Ok(Some(t)) => t,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": "User not found" })),
            )
                .into_response();
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response();
        }
    };

    if let Err(e) = state.user_store.set_user_phone(&user_id, &phone).await {
        let msg = e.to_string();
        let status = if msg.contains("already bound") {
            StatusCode::CONFLICT
        } else {
            StatusCode::INTERNAL_SERVER_ERROR
        };
        return (status, Json(serde_json::json!({ "error": msg }))).into_response();
    }

    tracing::info!(
        actor = %auth_user.user_id,
        user_id = %user_id,
        username = %target.username,
        phone = %mask_phone(&phone),
        "Admin assigned phone number"
    );

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "message": "Phone number assigned",
            "phone_masked": mask_phone(&phone),
            "phone_bound": true,
        })),
    )
        .into_response()
}

/// DELETE /api/admin/users/{id}/phone — unbind a phone number.
///
/// Frees the number so it can be bound to a different account, typically after
/// someone leaves. An account whose only credential was the number (provisioned
/// by SMS) cannot log in again until a new one is assigned; accounts with a
/// password or email keep their own login path.
pub(crate) async fn clear_user_phone(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Path(user_id): Path<String>,
) -> impl IntoResponse {
    if let Err(resp) = ensure_admin(&auth_user) {
        return resp;
    }

    // Refuse on the caller's own account: the number is the admin's SMS-login
    // channel, and the guard keeps it from being cut off by a stray click
    // (the admin password does not depend on the phone either way).
    if user_id == auth_user.user_id {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "Cannot remove your own phone number" })),
        )
            .into_response();
    }

    let target = match state.user_store.get_user_by_id(&user_id).await {
        Ok(Some(t)) => t,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": "User not found" })),
            )
                .into_response();
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response();
        }
    };

    if target.phone.is_none() {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "error": "User has no phone number bound" })),
        )
            .into_response();
    }

    if let Err(e) = state.user_store.clear_user_phone(&user_id).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response();
    }

    tracing::info!(
        actor = %auth_user.user_id,
        user_id = %user_id,
        username = %target.username,
        "Admin removed phone number; a phone-only account cannot log in until a new one is assigned"
    );

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "message": "Phone number removed",
            "phone_masked": "",
            "phone_bound": false,
        })),
    )
        .into_response()
}
