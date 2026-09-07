//! Authentication endpoints: register, login, forgot-password, reset-password, health.

use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use rand_core::{OsRng, RngCore};

use super::{AppState, ForgotPasswordRequest, LoginRequest, RegisterRequest, ResetCodeEntry, ResetPasswordRequest};
use crate::auth;

pub(crate) async fn health() -> &'static str {
    "ok"
}

pub(crate) async fn register(
    State(state): State<AppState>,
    Json(req): Json<RegisterRequest>,
) -> impl IntoResponse {
    // Check if username already exists
    match state.user_store.get_user_by_username(&req.username).await {
        Ok(Some(_)) => {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({"error": format!("Username '{}' already exists", req.username)})),
            );
        }
        Ok(None) => {},
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            );
        }
    }

    // Hash password and create user
    let hash = match auth::hash_password(&req.password) {
        Ok(h) => h,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            );
        }
    };

    let user = match state.user_store.create_user(&req.username, &hash).await {
        Ok(u) => u,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            );
        }
    };

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
        })),
    )
}

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

    // Verify password
    match auth::verify_password(&req.password, &user.password_hash) {
        Ok(true) => {},
        Ok(false) => {
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
        })),
    )
}

pub(crate) async fn forgot_password(
    State(state): State<AppState>,
    Json(req): Json<ForgotPasswordRequest>,
) -> impl IntoResponse {
    // Look up user by username
    let user = match state.user_store.get_user_by_username(&req.username).await {
        Ok(Some(u)) => u,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "User not found"})),
            );
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            );
        }
    };

    // Generate 6-digit reset code
    let mut bytes = [0u8; 4];
    OsRng.fill_bytes(&mut bytes);
    let num = u32::from_be_bytes(bytes) % 1_000_000;
    let code = format!("{:06}", num);

    // Store code valid for 10 minutes
    let expires_at = chrono::Utc::now().timestamp() + 600;
    state.reset_codes.insert(
        user.username.clone(),
        ResetCodeEntry {
            code: code.clone(),
            expires_at,
            user_id: user.id.clone(),
        },
    );

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "code": code,
            "expires_in": 600,
            "message": "Reset code generated. Use this code with your new password.",
        })),
    )
}

pub(crate) async fn reset_password(
    State(state): State<AppState>,
    Json(req): Json<ResetPasswordRequest>,
) -> impl IntoResponse {
    // Validate password length
    if req.password.len() < 6 {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "Password must be at least 6 characters"})),
        );
    }

    // Look up reset code
    let entry = match state.reset_codes.get(&req.username) {
        Some(entry) => entry.clone(),
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "Invalid or expired reset code. Please request a new one."})),
            );
        }
    };

    // Verify code
    if entry.code != req.code {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "Invalid reset code"})),
        );
    }

    // Check expiry
    if chrono::Utc::now().timestamp() > entry.expires_at {
        state.reset_codes.remove(&req.username);
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "Reset code has expired. Please request a new one."})),
        );
    }

    // Hash new password
    let hash = match auth::hash_password(&req.password) {
        Ok(h) => h,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            );
        }
    };

    // Update password
    if let Err(e) = state.user_store.update_password_hash(&entry.user_id, &hash).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        );
    }

    // Remove used code
    state.reset_codes.remove(&req.username);

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "message": "Password reset successfully. You can now log in with your new password.",
        })),
    )
}
