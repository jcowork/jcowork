//! Per-user backend settings.
//!
//! The web UI used to keep the selected model only in `localStorage`, which
//! left server-initiated conversations (the Feishu bot) on the deployment
//! default. [`set_model`] persists the choice so every backend turn can honor
//! it; `get_settings` lets the UI restore it on a fresh browser.

use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use serde::Deserialize;

use super::{AppState, AuthUser};

pub(crate) async fn get_settings(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
) -> impl IntoResponse {
    let model = state
        .user_settings
        .get_model(&auth_user.user_id)
        .await
        .unwrap_or(None);
    (StatusCode::OK, Json(serde_json::json!({ "model": model })))
}

#[derive(Debug, Deserialize)]
pub(crate) struct SetModelRequest {
    /// "provider:model" as selected in Settings; null/empty resets to the
    /// deployment default.
    model: Option<String>,
}

pub(crate) async fn set_model(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Json(req): Json<SetModelRequest>,
) -> impl IntoResponse {
    let model = req
        .model
        .as_deref()
        .map(str::trim)
        .filter(|m| !m.is_empty());

    // Reject a provider nobody configured: the same string would make every
    // backend-initiated turn fail, and the user would have no clue why.
    if let Some(model) = model {
        if state.llm_router.read().unwrap().get_provider(model).is_err() {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": format!("未知的模型或未配置的 provider：{}", model) })),
            );
        }
    }

    match state.user_settings.set_model(&auth_user.user_id, model).await {
        Ok(()) => (
            StatusCode::OK,
            Json(serde_json::json!({ "status": "saved", "model": model })),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        ),
    }
}
