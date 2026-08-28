//! Per-user Feishu configuration endpoints.

use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use serde::{Deserialize, Serialize};

use super::{mask_secret, AppState, AuthUser};

#[derive(Debug, Serialize)]
#[allow(dead_code)]
pub(crate) struct FeishuConfigResponse {
    app_id: String,
    app_secret_masked: String,
    verification_token: String,
    encrypt_key: String,
    is_configured: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct FeishuConfigRequest {
    app_id: String,
    app_secret: String,
    verification_token: String,
    encrypt_key: Option<String>,
}

pub(crate) async fn get_feishu_config(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
) -> impl IntoResponse {
    match state.feishu_config_store.get_by_user(&auth_user.user_id).await {
        Ok(Some(config)) => {
            let masked = mask_secret(&config.app_secret);
            (StatusCode::OK, Json(serde_json::json!({
                "app_id": config.app_id,
                "app_secret_masked": masked,
                "verification_token": config.verification_token,
                "encrypt_key": config.encrypt_key,
                "is_configured": true,
            })))
        }
        Ok(None) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "app_id": "",
                "app_secret_masked": "",
                "verification_token": "",
                "encrypt_key": "",
                "is_configured": false,
            })),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        ),
    }
}

pub(crate) async fn set_feishu_config(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Json(req): Json<FeishuConfigRequest>,
) -> impl IntoResponse {
    let encrypt_key = req.encrypt_key.unwrap_or_default();
    match state
        .feishu_config_store
        .upsert(
            &auth_user.user_id,
            &req.app_id,
            &req.app_secret,
            &req.verification_token,
            &encrypt_key,
        )
        .await
    {
        Ok(()) => {
            // Invalidate client cache for this app_id so a fresh client is created next time
            state.feishu_client_cache.remove(&req.app_id);
            (StatusCode::OK, Json(serde_json::json!({ "status": "saved" })))
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e.to_string() })),
        ),
    }
}

pub(crate) async fn delete_feishu_config(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
) -> impl IntoResponse {
    // Get current config to know which app_id cache to invalidate
    if let Ok(Some(config)) = state.feishu_config_store.get_by_user(&auth_user.user_id).await {
        state.feishu_client_cache.remove(&config.app_id);
    }
    match state.feishu_config_store.delete(&auth_user.user_id).await {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({ "status": "deleted" }))),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        ),
    }
}
