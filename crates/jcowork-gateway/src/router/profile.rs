//! Self-service account profile: phone binding and display name.
//!
//! Every handler re-reads the account from the database rather than trusting the
//! JWT claims, so a page reload cannot present a stale "phone already bound"
//! state to the forced-binding screen.

use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use jcowork_storage::sms_only_password_hash;
use jcowork_storage::user_store::User;

use super::auth_api::consume_sms_code;
use super::{AppState, AuthUser, BindPhoneRequest, ChangeUsernameRequest, mask_phone};
use crate::auth::normalize_phone;

/// Shortest and longest accepted display name, in characters.
const USERNAME_MIN_CHARS: usize = 2;
const USERNAME_MAX_CHARS: usize = 24;

pub(crate) async fn get_profile(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
) -> impl IntoResponse {
    match state.user_store.get_user_by_id(&auth_user.user_id).await {
        Ok(Some(user)) => (StatusCode::OK, Json(profile_json(&user))),
        Ok(None) => (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "Account not found"})),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ),
    }
}

/// Bind (or rebind) the caller's phone number after verifying an SMS code.
///
/// On success the account's password hash is replaced with the SMS-only
/// sentinel. That is what closes the bootstrap password door for `admin`: the
/// default password is a published constant, so leaving it verifiable after a
/// phone exists would let anyone who reaches the port take the instance over.
pub(crate) async fn bind_phone(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Json(req): Json<BindPhoneRequest>,
) -> impl IntoResponse {
    let phone = match normalize_phone(&req.phone) {
        Some(p) => p,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "Invalid phone number"})),
            );
        }
    };

    if let Err(resp) = consume_sms_code(&state, &phone, req.code.trim()) {
        return resp;
    }

    if let Err(e) = state.user_store.set_user_phone(&auth_user.user_id, &phone).await {
        let msg = e.to_string();
        let status = if msg.contains("already bound") {
            StatusCode::CONFLICT
        } else {
            StatusCode::INTERNAL_SERVER_ERROR
        };
        return (status, Json(serde_json::json!({"error": msg})));
    }

    if let Err(e) = state
        .user_store
        .update_password_hash(&auth_user.user_id, &sms_only_password_hash())
        .await
    {
        tracing::error!(error = %e, "Failed to close password login after phone binding");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        );
    }

    tracing::info!(
        user_id = %auth_user.user_id,
        phone = %mask_phone(&phone),
        "Phone number bound; password login disabled for this account"
    );

    match state.user_store.get_user_by_id(&auth_user.user_id).await {
        Ok(Some(user)) => (StatusCode::OK, Json(profile_json(&user))),
        Ok(None) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": "Account not found"})),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ),
    }
}

pub(crate) async fn change_username(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Json(req): Json<ChangeUsernameRequest>,
) -> impl IntoResponse {
    let name = req.username.trim();

    let chars = name.chars().count();
    if !(USERNAME_MIN_CHARS..=USERNAME_MAX_CHARS).contains(&chars) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": format!(
                    "Username must be between {} and {} characters",
                    USERNAME_MIN_CHARS, USERNAME_MAX_CHARS
                ),
            })),
        );
    }

    // The name is typed into `@`-mentions and rendered into markdown, so the
    // characters that would break either are rejected outright.
    if name.chars().any(|c| c.is_whitespace() || c.is_control() || matches!(c, '@' | '/' | '\\')) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "Username cannot contain spaces, @, / or control characters",
            })),
        );
    }

    match state.user_store.update_username(&auth_user.user_id, name).await {
        Ok(()) => match state.user_store.get_user_by_id(&auth_user.user_id).await {
            Ok(Some(user)) => (StatusCode::OK, Json(profile_json(&user))),
            Ok(None) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "Account not found"})),
            ),
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            ),
        },
        Err(e) => {
            let msg = e.to_string();
            let status = if msg.contains("already exists") {
                StatusCode::CONFLICT
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            (status, Json(serde_json::json!({"error": msg})))
        }
    }
}

/// Shape of every profile response. `password_hash` is never included.
fn profile_json(user: &User) -> serde_json::Value {
    serde_json::json!({
        "username": user.username,
        "phone_masked": user.phone.as_deref().map(mask_phone).unwrap_or_default(),
        "phone_bound": user.phone.is_some(),
        "is_admin": user.is_admin,
    })
}
