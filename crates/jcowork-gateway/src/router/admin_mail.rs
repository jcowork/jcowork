//! Admin management of the mail channel: read the live configuration, replace
//! it at runtime, reset to the environment's, and send a test message.
//!
//! The stored configuration (in `app_settings`) wins over `JCWORK_MAIL_*` on
//! the next startup, which is what lets a desktop deployment — no `.env`
//! file at all — run a real SMTP channel. The SMTP password is never
//! returned; `GET` reports only whether one is set.

use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use serde::Deserialize;

use super::{AppState, AuthUser, MailRuntime};
use super::admin_users::ensure_admin;
use crate::auth::normalize_email;

/// `app_settings` key holding the stored mail configuration as JSON.
pub(crate) const MAIL_SETTINGS_KEY: &str = "mail";

/// Fields are all optional: absent means "keep the current value", and an
/// empty `smtp_password` keeps the stored password (the UI never echoes it).
#[derive(Debug, Deserialize)]
pub(crate) struct UpdateMailRequest {
    pub provider: Option<String>,
    pub smtp_host: Option<String>,
    pub smtp_port: Option<u16>,
    pub smtp_username: Option<String>,
    pub smtp_password: Option<String>,
    pub smtp_from: Option<String>,
    pub daily_global_cap: Option<u32>,
    pub allow_register: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct MailTestRequest {
    pub to: String,
}

/// The redaction boundary for every response: config fields plus
/// `password_set`, never `smtp_password` itself.
fn mail_config_json(config: &jcowork_mail::config::MailConfig, source: &str) -> serde_json::Value {
    let resolved = match config.resolve() {
        Ok(jcowork_mail::config::MailProviderKind::Smtp) => "smtp",
        Ok(jcowork_mail::config::MailProviderKind::Dev) => "dev",
        Err(_) => "invalid",
    };
    serde_json::json!({
        "provider": config.provider,
        "smtp_host": config.smtp_host,
        "smtp_port": config.smtp_port,
        "smtp_username": config.smtp_username,
        // The password is deliberately absent; only its existence is exposed.
        "password_set": !config.smtp_password.is_empty(),
        "smtp_from": config.smtp_from,
        "daily_global_cap": config.daily_global_cap,
        "allow_register": config.allow_register,
        "resolved": resolved,
        "auto_dev": config.is_auto_dev(),
        "source": source,
    })
}

/// GET /api/admin/mail — the live channel's configuration, redacted.
pub(crate) async fn get_mail_config(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
) -> axum::response::Response {
    if let Err(resp) = ensure_admin(&auth_user) {
        return resp;
    }
    let stored = state
        .app_settings
        .get(MAIL_SETTINGS_KEY)
        .await
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<jcowork_mail::config::MailConfig>(&raw).ok());
    let mail = state.mail.read().expect("mail runtime lock poisoned");
    let source = if stored.is_some_and(|c| c == mail.config) {
        "stored"
    } else {
        "env"
    };
    (StatusCode::OK, Json(mail_config_json(&mail.config, source))).into_response()
}

/// PUT /api/admin/mail — merge the patch into the live config, persist it,
/// and rebuild the channel in place.
///
/// Refuses `provider = "smtp"` with incomplete credentials so a typo cannot
/// silently downgrade the channel to dev (the same fail-fast rule as
/// startup). Persistence happens only after the new runtime builds, so a
/// broken input leaves the running channel untouched.
pub(crate) async fn update_mail_config(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Json(req): Json<UpdateMailRequest>,
) -> axum::response::Response {
    if let Err(resp) = ensure_admin(&auth_user) {
        return resp;
    }

    let mut config = state.mail.read().expect("mail runtime lock poisoned").config.clone();
    if let Some(provider) = req.provider {
        config.provider = provider.trim().to_lowercase();
    }
    if let Some(v) = req.smtp_host {
        config.smtp_host = v.trim().to_string();
    }
    if let Some(v) = req.smtp_port {
        config.smtp_port = v;
    }
    if let Some(v) = req.smtp_username {
        config.smtp_username = v.trim().to_string();
    }
    // An empty string means "leave the password alone": the UI can never
    // echo the stored secret, so re-submitting the form without retyping it
    // must not wipe it.
    if let Some(v) = req.smtp_password {
        if !v.is_empty() {
            config.smtp_password = v;
        }
    }
    if let Some(v) = req.smtp_from {
        config.smtp_from = v.trim().to_string();
    }
    if let Some(v) = req.daily_global_cap {
        config.daily_global_cap = v;
    }
    if let Some(v) = req.allow_register {
        config.allow_register = v;
    }

    if config.provider == "smtp" {
        if config.smtp_host.is_empty()
            || config.smtp_username.is_empty()
            || config.smtp_password.is_empty()
            || config.smtp_from.is_empty()
        {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "SMTP requires host, username, password and from to be set",
                })),
            )
                .into_response();
        }
    }
    if let Some(e) = store_config(&state, &config).await {
        return e;
    }

    let runtime = match MailRuntime::from_config(&config) {
        Ok(r) => r,
        Err(e) => {
            // Reverting the stored row keeps the next startup on the last
            // known-good config; the live channel was never touched.
            let _ = state.app_settings.remove(MAIL_SETTINGS_KEY).await;
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };
    *state.mail.write().expect("mail runtime lock poisoned") = runtime;

    tracing::info!(actor = %auth_user.user_id, "Admin updated the mail channel");
    let mail = state.mail.read().expect("mail runtime lock poisoned");
    (StatusCode::OK, Json(mail_config_json(&mail.config, "stored"))).into_response()
}

/// DELETE /api/admin/mail — drop the stored configuration and fall back to
/// whatever `JCWORK_MAIL_*` says (dev fallback on a machine with no env).
pub(crate) async fn delete_mail_config(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
) -> axum::response::Response {
    if let Err(resp) = ensure_admin(&auth_user) {
        return resp;
    }
    if let Err(e) = state.app_settings.remove(MAIL_SETTINGS_KEY).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response();
    }
    let runtime = match MailRuntime::from_env() {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };
    *state.mail.write().expect("mail runtime lock poisoned") = runtime;

    tracing::info!(actor = %auth_user.user_id, "Admin removed the stored mail config; env applies again");
    let mail = state.mail.read().expect("mail runtime lock poisoned");
    (StatusCode::OK, Json(mail_config_json(&mail.config, "env"))).into_response()
}

/// POST /api/admin/mail/test — send a probe message through the live channel.
///
/// The fake `test:<address>` key reuses the CodeStore's cooldown/daily-cap
/// rules without ever colliding with a real verification code (consume looks
/// up the bare address), so the probe can't be turned into an oracle or a
/// spam relay beyond what one verification code would cost.
pub(crate) async fn send_test_mail(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Json(req): Json<MailTestRequest>,
) -> axum::response::Response {
    if let Err(resp) = ensure_admin(&auth_user) {
        return resp;
    }
    let Some(to) = normalize_email(&req.to) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "Invalid email address"})),
        )
            .into_response();
    };

    let mail = state.mail.read().expect("mail runtime lock poisoned").clone();
    let probe_key = format!("test:{to}");
    let (code, pending) = match mail.store.issue(&probe_key) {
        Ok(issued) => issued,
        Err(e) => return e.into_response("Email", "email address").into_response(),
    };
    if let Err(e) = mail.sender.send_code(&to, &code).await {
        drop(pending);
        return (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({
                "error": format!("Failed to send the test message: {e}"),
            })),
        )
            .into_response();
    }
    pending.commit();

    tracing::info!(actor = %auth_user.user_id, to = %to, "Admin sent a mail-channel test message");
    (StatusCode::OK, Json(serde_json::json!({"message": "Test message sent"}))).into_response()
}

async fn store_config(
    state: &AppState,
    config: &jcowork_mail::config::MailConfig,
) -> Option<axum::response::Response> {
    let raw = match serde_json::to_string(config) {
        Ok(r) => r,
        Err(e) => {
            return Some((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response());
        }
    };
    if let Err(e) = state.app_settings.set(MAIL_SETTINGS_KEY, &raw).await {
        return Some((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response());
    }
    None
}
