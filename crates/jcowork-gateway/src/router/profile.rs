//! Self-service account profile: phone binding, admin password change, and
//! display name.
//!
//! Every handler re-reads the account from the database rather than trusting the
//! JWT claims, so a page reload cannot present a stale "phone already bound"
//! state to the binding screen.

use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use jcowork_storage::user_store::User;

use super::admin_users::ensure_admin;
use super::auth_api::consume_sms_code;
use super::{AppState, AuthUser, BindPhoneRequest, ChangePasswordRequest, ChangeUsernameRequest, mask_email, mask_phone};
use crate::auth::{self, normalize_phone, password_error};

/// Shortest and longest accepted display name, in characters.
const USERNAME_MIN_CHARS: usize = 2;
const USERNAME_MAX_CHARS: usize = 24;

pub(crate) async fn get_profile(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
) -> impl IntoResponse {
    match state.user_store.get_user_by_id(&auth_user.user_id).await {
        Ok(Some(user)) => (StatusCode::OK, Json(profile_json(&state, &user))),
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
/// Binding is optional contact info — it adds SMS login but never disables
/// the caller's own credential (the admin's password, an email account's
/// password), so the password hash is left untouched.
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

    tracing::info!(
        user_id = %auth_user.user_id,
        phone = %mask_phone(&phone),
        "Phone number bound"
    );

    match state.user_store.get_user_by_id(&auth_user.user_id).await {
        Ok(Some(user)) => (StatusCode::OK, Json(profile_json(&state, &user))),
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

/// Replace the admin's password, lifting the bootstrap gate.
///
/// The caller is token-authenticated (a bootstrap login, or SMS for a
/// `JCWORK_ADMIN_PHONE` admin who may not even know the bootstrap password),
/// so only the new password is submitted. Non-admins are refused: email
/// accounts already have a code-verified reset path.
pub(crate) async fn change_password(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Json(req): Json<ChangePasswordRequest>,
) -> axum::response::Response {
    if let Err(resp) = ensure_admin(&auth_user) {
        return resp;
    }

    if let Some(msg) = password_error(&req.new_password) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": msg})),
        )
            .into_response();
    }

    let hash = match auth::hash_password(&req.new_password) {
        Ok(h) => h,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };

    // Also stamps password_changed_at, which is what lifts the middleware
    // bootstrap gate on the next request.
    if let Err(e) = state.user_store.update_password_hash(&auth_user.user_id, &hash).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response();
    }

    tracing::info!(user_id = %auth_user.user_id, "Admin password changed");

    match state.user_store.get_user_by_id(&auth_user.user_id).await {
        Ok(Some(user)) => (StatusCode::OK, Json(profile_json(&state, &user))).into_response(),
        Ok(None) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": "Account not found"})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
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
            Ok(Some(user)) => (StatusCode::OK, Json(profile_json(&state, &user))),
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
fn profile_json(state: &AppState, user: &User) -> serde_json::Value {
    let must_configure_mail = user.is_admin
        && state
            .mail
            .read()
            .expect("mail runtime lock poisoned")
            .auto_dev;
    serde_json::json!({
        "username": user.username,
        "phone_masked": user.phone.as_deref().map(mask_phone).unwrap_or_default(),
        "phone_bound": user.phone.is_some(),
        "email_masked": user.email.as_deref().map(mask_email).unwrap_or_default(),
        "email_bound": user.email.is_some(),
        "is_admin": user.is_admin,
        // Drives the client's forced change-password screen after a reload.
        "must_change_password": user.is_admin && user.password_changed_at.is_none(),
        // Drives the admin's forced mail-setup screen after a reload.
        "must_configure_mail": must_configure_mail,
    })
}
