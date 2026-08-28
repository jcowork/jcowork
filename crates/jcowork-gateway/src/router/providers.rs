//! LLM provider management endpoints.

use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use serde::Deserialize;

use super::{mask_secret, AppState, AuthUser};
use jcowork_llm::{LlmRouter, ProviderEntry};

pub(crate) async fn list_providers(
    State(state): State<AppState>,
    _auth: axum::Extension<AuthUser>,
) -> impl IntoResponse {
    let router = state.llm_router.read().unwrap();
    let providers = router.providers_info();
    let default_model = &state.default_model;
    (StatusCode::OK, Json(serde_json::json!({
        "providers": providers,
        "default_model": default_model,
    })))
}

/// GET /api/providers/entries - returns full provider entries (with api_key masked).
pub(crate) async fn list_provider_entries(
    State(state): State<AppState>,
    _auth: axum::Extension<AuthUser>,
) -> impl IntoResponse {
    let providers_file = LlmRouter::providers_file_path(&state.data_dir);
    let entries = LlmRouter::load_entries_from_file(&providers_file)
        .unwrap_or_default();
    // Mask API keys in the response
    let masked: Vec<serde_json::Value> = entries.iter().map(|e| {
        serde_json::json!({
            "id": e.id,
            "name": e.name,
            "api_key": mask_secret(&e.api_key),
            "api_key_set": !e.api_key.is_empty(),
            "base_url": e.base_url,
            "default_model": e.default_model,
            "context_length": e.context_length,
            "models": e.models,
        })
    }).collect();
    (StatusCode::OK, Json(serde_json::json!({ "entries": masked })))
}

#[derive(Debug, Deserialize)]
pub(crate) struct SaveProvidersRequest {
    pub entries: Vec<ProviderEntry>,
}

/// POST /api/providers - save all provider entries and rebuild the router.
pub(crate) async fn save_providers(
    State(state): State<AppState>,
    _auth: axum::Extension<AuthUser>,
    Json(req): Json<SaveProvidersRequest>,
) -> impl IntoResponse {
    let providers_file = LlmRouter::providers_file_path(&state.data_dir);

    // Save to disk
    if let Err(e) = LlmRouter::save_entries_to_file(&providers_file, &req.entries) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("Failed to save providers: {}", e) })),
        );
    }

    // Rebuild the router
    let new_router = LlmRouter::rebuild_from_entries(&req.entries);
    let mut router = state.llm_router.write().unwrap();
    *router = new_router;

    tracing::info!(count = req.entries.len(), "Providers saved and router rebuilt");
    (StatusCode::OK, Json(serde_json::json!({ "status": "saved", "count": req.entries.len() })))
}
