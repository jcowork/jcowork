//! Admin management of the voice channel: read the live configuration,
//! replace it at runtime, and reset to the environment's.
//!
//! Mirrors [`super::admin_mail`]: the stored configuration (in `app_settings`)
//! wins over `DASHSCOPE_API_KEY` / `JCWORK_VOICE_*` on the next startup, which
//! lets a desktop deployment — no `.env` file at all — run voice calls. The
//! API key is never returned; `GET` reports only whether one is set.

use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use serde::Deserialize;

use super::{AppState, AuthUser};
use super::admin_users::ensure_admin;
use jcowork_voice::VoiceConfig;

/// `app_settings` key holding the stored voice configuration as JSON.
pub(crate) const VOICE_SETTINGS_KEY: &str = "voice";

/// Fields are all optional: absent means "keep the current value", and an
/// empty `api_key` keeps the stored key (the UI never echoes it).
#[derive(Debug, Deserialize)]
pub(crate) struct UpdateVoiceRequest {
    pub api_key: Option<String>,
    pub workspace_id: Option<String>,
    pub asr_model: Option<String>,
    pub tts_model: Option<String>,
    pub tts_voice: Option<String>,
    pub asr_url: Option<String>,
    pub tts_url: Option<String>,
    /// "cloud" or "local"; cloud sessions fall back to local automatically.
    pub backend: Option<String>,
    pub local_asr_url: Option<String>,
    pub local_tts_url: Option<String>,
    pub local_voice: Option<String>,
}

/// Redacted view of the live configuration — the API key itself never leaves
/// the server, only whether one is set.
fn voice_config_json(config: &VoiceConfig, source: &str) -> serde_json::Value {
    serde_json::json!({
        "api_key_set": !config.api_key.is_empty(),
        "workspace_id": config.workspace_id,
        "asr_model": config.asr_model,
        "tts_model": config.tts_model,
        "tts_voice": config.tts_voice,
        "asr_url": config.asr_url,
        "tts_url": config.tts_url,
        "backend": config.backend,
        "local_asr_url": config.local_asr_url,
        "local_tts_url": config.local_tts_url,
        "local_voice": config.local_voice,
        "cloud_configured": config.cloud_configured(),
        "enabled": config.is_configured(),
        "source": source,
    })
}

/// GET /api/admin/voice — the live channel's configuration, redacted.
pub(crate) async fn get_voice_config(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
) -> axum::response::Response {
    if let Err(resp) = ensure_admin(&auth_user) {
        return resp;
    }
    let stored = state
        .app_settings
        .get(VOICE_SETTINGS_KEY)
        .await
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<VoiceConfig>(&raw).ok());
    let config = state.voice.read().expect("voice config lock poisoned");
    let source = if stored.is_some() { "stored" } else { "env" };
    (StatusCode::OK, Json(voice_config_json(&config, source))).into_response()
}

/// PUT /api/admin/voice — merge the patch into the live config, persist it,
/// and swap the live configuration in place (no restart needed).
///
/// When the workspace id or a model changes and the patch does not carry an
/// explicit URL override, the corresponding default endpoint is re-derived —
/// otherwise a workspace change would keep pointing at the old workspace.
pub(crate) async fn update_voice_config(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Json(req): Json<UpdateVoiceRequest>,
) -> axum::response::Response {
    if let Err(resp) = ensure_admin(&auth_user) {
        return resp;
    }
    let mut config = state.voice.read().expect("voice config lock poisoned").clone();

    // An empty string means "leave the key alone": the UI can never echo the
    // stored secret, so re-submitting the form without retyping it must not
    // wipe it.
    if let Some(v) = req.api_key {
        if !v.trim().is_empty() {
            config.api_key = v.trim().to_string();
        }
    }
    if let Some(v) = req.workspace_id {
        config.workspace_id = v.trim().to_string();
    }
    if let Some(v) = req.asr_model {
        if !v.trim().is_empty() {
            config.asr_model = v.trim().to_string();
        }
    }
    if let Some(v) = req.tts_model {
        if !v.trim().is_empty() {
            config.tts_model = v.trim().to_string();
        }
    }
    if let Some(v) = req.tts_voice {
        if !v.trim().is_empty() {
            config.tts_voice = v.trim().to_string();
        }
    }
    if let Some(v) = req.backend {
        let v = v.trim().to_ascii_lowercase();
        if v != "cloud" && v != "local" {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "backend must be cloud or local"})),
            )
                .into_response();
        }
        config.backend = v;
    }
    if let Some(v) = req.local_asr_url {
        config.local_asr_url = v.trim().to_string();
    }
    if let Some(v) = req.local_tts_url {
        config.local_tts_url = v.trim().to_string();
    }
    if let Some(v) = req.local_voice {
        if !v.trim().is_empty() {
            config.local_voice = v.trim().to_string();
        }
    }
    // Explicit URL overrides win; otherwise keep the URLs in sync with the
    // workspace/model they were derived from.
    match req.asr_url {
        Some(v) if !v.trim().is_empty() => config.asr_url = v.trim().to_string(),
        _ => config.asr_url = VoiceConfig::default_asr_url(&config.workspace_id, &config.asr_model),
    }
    match req.tts_url {
        Some(v) if !v.trim().is_empty() => config.tts_url = v.trim().to_string(),
        _ => config.tts_url = VoiceConfig::default_tts_url(),
    }

    let raw = match serde_json::to_string(&config) {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };
    if let Err(e) = state.app_settings.set(VOICE_SETTINGS_KEY, &raw).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response();
    }
    *state.voice.write().expect("voice config lock poisoned") = config.clone();

    tracing::info!(actor = %auth_user.user_id, "Admin updated the voice channel");
    (StatusCode::OK, Json(voice_config_json(&config, "stored"))).into_response()
}

/// DELETE /api/admin/voice — drop the stored configuration and fall back to
/// whatever `DASHSCOPE_API_KEY` / `JCWORK_VOICE_*` say.
pub(crate) async fn delete_voice_config(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
) -> axum::response::Response {
    if let Err(resp) = ensure_admin(&auth_user) {
        return resp;
    }
    if let Err(e) = state.app_settings.remove(VOICE_SETTINGS_KEY).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response();
    }
    let config = VoiceConfig::from_env();
    *state.voice.write().expect("voice config lock poisoned") = config.clone();

    tracing::info!(actor = %auth_user.user_id, "Admin removed the stored voice config; env applies again");
    (StatusCode::OK, Json(voice_config_json(&config, "env"))).into_response()
}