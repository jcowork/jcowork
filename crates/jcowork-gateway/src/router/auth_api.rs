//! Authentication endpoints: SMS verification-code login, admin bootstrap
//! password login, and health.

use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};

use super::code_store::{ConsumeError, CODE_TTL_SECS, RESEND_COOLDOWN_SECS};
use super::{AppState, LoginRequest, SmsLoginRequest, SmsSendRequest, mask_phone};
use crate::auth;
use crate::auth::normalize_phone;

pub(crate) async fn health() -> &'static str {
    "ok"
}

/// Password login, kept only as the one-time bootstrap door for `admin`.
pub(crate) async fn login(
    State(state): State<AppState>,
    Json(req): Json<LoginRequest>,
) -> impl IntoResponse {
    // Look up user by username
    let user = match state.user_store.get_user_by_username(&req.username).await {
        Ok(Some(u)) => u,
        Ok(None) => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "Invalid username or password"})),
            );
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            );
        }
    };

    // Recycle-bin accounts cannot log in until restored by the admin.
    if user.deleted_at.is_some() {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": "Account has been deleted"})),
        );
    }

    // Password login exists solely to let a freshly seeded admin reach the
    // phone-binding screen. Check this before verifying the password: the
    // response has to be identical to a wrong password, or it would disclose
    // which usernames hold admin accounts.
    if !user.is_admin || user.phone.is_some() {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "Invalid username or password"})),
        );
    }

    // Verify password
    match auth::verify_password(&req.password, &user.password_hash) {
        Ok(true) => {},
        // An unparseable hash is not a server fault: SMS-only accounts hold a
        // deliberate sentinel that Argon2 rejects. Reporting it as invalid
        // credentials keeps password login failing closed without revealing
        // that the account authenticates some other way.
        Ok(false) | Err(_) => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "Invalid username or password"})),
            );
        }
    }

    // Create JWT token
    let token = match auth::create_token(&state.auth_config, &user.id, &user.username) {
        Ok(t) => t,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            );
        }
    };

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "token": token,
            "user_id": user.id,
            "username": user.username,
            "is_admin": user.is_admin,
            // Always true here — the gate above only admits accounts without a
            // phone — but the frontend branches on it, so state it explicitly.
            "must_bind_phone": true,
        })),
    )
}

// --- SMS verification-code login ---

/// Send a verification code to a phone number.
///
/// The response body deliberately omits both the code and the provider name.
/// In dev mode the code exists only in the server log; echoing it — or even
/// revealing that the channel is `dev` — would tell an attacker exactly where
/// to look.
pub(crate) async fn sms_send(
    State(state): State<AppState>,
    Json(req): Json<SmsSendRequest>,
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

    // Reserve the throttling budget and stage the code. Every refusal path
    // (cooldown, lockout, caps) releases the budget internally and maps to the
    // exact same response this handler has always produced.
    let (code, pending) = match state.sms.store.issue(&phone) {
        Ok(issued) => issued,
        Err(e) => return e.into_response("SMS", "phone number"),
    };

    // `pending` stages the code but commits nothing: if the carrier rejects
    // the send, dropping it releases the budget slot and restores the phone's
    // previous entry, so the failed attempt costs neither side anything.
    if let Err(e) = state.sms.sender.send_code(&phone, &code).await {
        drop(pending);
        tracing::error!(error = %e, phone = %mask_phone(&phone), "SMS send failed");
        return (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({
                "error": "Failed to send verification code, please try again later",
            })),
        );
    }

    pending.commit();

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "expires_in": CODE_TTL_SECS,
            "resend_after": RESEND_COOLDOWN_SECS,
        })),
    )
}

/// Exchange a verified phone number + code for a session token.
///
/// An unknown number auto-provisions an account unless `sms_allow_register` is
/// off, which is what makes this endpoint the only signup path.
pub(crate) async fn sms_login(
    State(state): State<AppState>,
    Json(req): Json<SmsLoginRequest>,
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

    let submitted = req.code.trim();
    if let Err(resp) = consume_sms_code(&state, &phone, submitted) {
        return resp;
    }

    let user = match state.user_store.get_user_by_phone(&phone).await {
        Ok(Some(u)) => u,
        Ok(None) => {
            if !state.sms.allow_register {
                return (
                    StatusCode::FORBIDDEN,
                    Json(serde_json::json!({"error": "Phone number is not registered"})),
                );
            }
            match state.user_store.get_or_create_by_phone(&phone).await {
                Ok(u) => u,
                Err(e) => {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(serde_json::json!({"error": e.to_string()})),
                    );
                }
            }
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            );
        }
    };

    // A trashed account still occupies its number in the unique index, so the
    // lookup above finds it; reject here instead of handing out a token that the
    // auth middleware would immediately bounce.
    if user.deleted_at.is_some() {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": "Account has been deleted"})),
        );
    }

    let token = match auth::create_token(&state.auth_config, &user.id, &user.username) {
        Ok(t) => t,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            );
        }
    };

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "token": token,
            "user_id": user.id,
            "username": user.username,
            "is_admin": user.is_admin,
            "must_bind_phone": user.phone.is_none(),
        })),
    )
}

/// Verify `submitted` against the outstanding code for `phone` and consume it.
///
/// Shared by SMS login and phone binding so both paths enforce identical
/// attempt limits. On `Ok` the code is spent; on `Err` the caller returns the
/// response verbatim.
pub(crate) fn consume_sms_code(
    state: &AppState,
    phone: &str,
    submitted: &str,
) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
    state
        .sms
        .store
        .consume(phone, submitted)
        .map_err(ConsumeError::into_response)
}

