//! RSI (recursive self-improvement) endpoints.
//!
//! The nav switch reads/writes the per-account opt-in flag here; the daily
//! scheduler ([`crate::rsi`]) does the actual work. `POST /api/rsi/run`
//! triggers a run immediately for the calling account — for the admin that
//! means a deployment-wide usage analysis, for everyone else a workspace and
//! periodic-plan review.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::Deserialize;
use std::path::PathBuf;

use super::admin_users::ensure_admin;
use super::{AppState, AuthUser};
use crate::rsi;

#[derive(Debug, Deserialize)]
pub(crate) struct ReportsQuery {
    limit: Option<i64>,
}

/// Source config exposed by `/api/rsi`: the configured override (`None` =
/// auto-detect) and the path a run would actually use. Personal accounts
/// never see either.
async fn source_fields(state: &AppState, is_admin: bool) -> (Option<String>, Option<String>) {
    if !is_admin {
        return (None, None);
    }
    state.rsi.source_config().await
}

fn source_payload(configured: Option<String>, resolved: Option<String>) -> serde_json::Value {
    serde_json::json!({
        "source_configured": configured,
        "source_root": resolved,
    })
}

/// GET /api/rsi — toggle state plus the role the next run will follow.
pub(crate) async fn get_status(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
) -> impl IntoResponse {
    let enabled = state.rsi.enabled(&auth_user.user_id).await;
    let last_report_at = state.rsi.last_report_at(&auth_user.user_id).await;
    let (source_configured, source_root) = source_fields(&state, auth_user.is_admin).await;
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "enabled": enabled,
            "role": if auth_user.is_admin { "admin" } else { "user" },
            "running": state.rsi.is_running(&auth_user.user_id),
            "last_report_at": last_report_at,
            "schedule_hour": rsi::schedule_hour(),
            "source_root": source_root,
            "source_configured": source_configured,
        })),
    )
}

#[derive(Debug, Deserialize)]
pub(crate) struct ToggleRequest {
    enabled: bool,
}

/// PUT /api/rsi — turn the daily run on or off.
pub(crate) async fn update_enabled(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Json(req): Json<ToggleRequest>,
) -> impl IntoResponse {
    match state.rsi.set_enabled(&auth_user.user_id, req.enabled).await {
        Ok(()) => {
            tracing::info!(
                user_id = %auth_user.user_id,
                enabled = req.enabled,
                "RSI toggle updated"
            );
            let (source_configured, source_root) = source_fields(&state, auth_user.is_admin).await;
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "enabled": req.enabled,
                    "role": if auth_user.is_admin { "admin" } else { "user" },
                    "running": state.rsi.is_running(&auth_user.user_id),
                    "last_report_at": state.rsi.last_report_at(&auth_user.user_id).await,
                    "schedule_hour": rsi::schedule_hour(),
                    "source_root": source_root,
                    "source_configured": source_configured,
                })),
            )
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        ),
    }
}

/// GET /api/rsi/reports — newest runs, without their full output.
pub(crate) async fn list_reports(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Query(query): Query<ReportsQuery>,
) -> impl IntoResponse {
    match state
        .rsi
        .list_reports(&auth_user.user_id, query.limit.unwrap_or(20))
        .await
    {
        Ok(reports) => (StatusCode::OK, Json(serde_json::json!(reports))),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        ),
    }
}

/// GET /api/rsi/reports/{id} — one run including the full answer.
pub(crate) async fn get_report(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.rsi.get_report(&auth_user.user_id, &id).await {
        Ok(Some(report)) => (StatusCode::OK, Json(serde_json::json!(report))),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "Report not found" })),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        ),
    }
}

/// POST /api/rsi/run — start an analysis for the calling account now.
pub(crate) async fn run_now(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
) -> impl IntoResponse {
    let user = match state.user_store.get_user_by_id(&auth_user.user_id).await {
        Ok(Some(user)) if user.deleted_at.is_none() => user,
        Ok(_) => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": "Account not found" })),
            )
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
        }
    };

    if state.rsi.run_now(user) {
        (StatusCode::ACCEPTED, Json(serde_json::json!({ "status": "started" })))
    } else {
        (
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "error": "An RSI run is already in progress" })),
        )
    }
}

/// GET /api/rsi/source — the admin-configured checkout and the resolved path.
pub(crate) async fn get_source(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
) -> axum::response::Response {
    if let Err(resp) = ensure_admin(&auth_user) {
        return resp;
    }
    let (configured, resolved) = state.rsi.source_config().await;
    (StatusCode::OK, Json(source_payload(configured, resolved))).into_response()
}

#[derive(Debug, Deserialize)]
pub(crate) struct SourceRequest {
    /// Absolute path to the repository root; empty resets to auto-detection.
    path: String,
}

/// PUT /api/rsi/source — pin or clear the checkout an admin run reads as
/// `source/`.
///
/// The path must be an existing directory holding a `Cargo.toml`, so a typo
/// cannot silently widen the read-only tree to an unrelated directory. An
/// empty path removes the override and falls back to auto-detection.
pub(crate) async fn update_source(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Json(req): Json<SourceRequest>,
) -> axum::response::Response {
    if let Err(resp) = ensure_admin(&auth_user) {
        return resp;
    }

    let trimmed = req.path.trim();
    if trimmed.is_empty() {
        if let Err(e) = state.app_settings.remove(rsi::SOURCE_SETTINGS_KEY).await {
            return internal_error(e);
        }
        tracing::info!(actor = %auth_user.user_id, "Admin cleared the RSI source directory; auto-detect applies");
    } else {
        let raw = PathBuf::from(trimmed);
        if !raw.is_absolute() {
            return bad_request("The source directory must be an absolute path");
        }
        let path = match raw.canonicalize() {
            Ok(p) => p,
            Err(_) => return bad_request("The source directory does not exist or is not accessible"),
        };
        if !path.is_dir() {
            return bad_request("The source directory must be a directory");
        }
        if !path.join("Cargo.toml").is_file() {
            return bad_request("The source directory must be a Jcowork repository root (Cargo.toml missing)");
        }
        let stored = path.to_string_lossy().to_string();
        if let Err(e) = state.app_settings.set(rsi::SOURCE_SETTINGS_KEY, &stored).await {
            return internal_error(e);
        }
        tracing::info!(actor = %auth_user.user_id, path = %stored, "Admin pinned the RSI source directory");
    }

    let (configured, resolved) = state.rsi.source_config().await;
    (StatusCode::OK, Json(source_payload(configured, resolved))).into_response()
}

fn bad_request(message: &str) -> axum::response::Response {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({ "error": message })),
    )
        .into_response()
}

fn internal_error(e: anyhow::Error) -> axum::response::Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({ "error": e.to_string() })),
    )
        .into_response()
}
