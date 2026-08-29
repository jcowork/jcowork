//! Connector management endpoints (API + MCP tool integrations).

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::Deserialize;

use super::{AppState, AuthUser};
use jcowork_connectors::models::{ApiConnectorConfig, Connector, McpConfig, TYPE_API, TYPE_MCP};

/// Request body for creating or updating a connector.
#[derive(Debug, Deserialize)]
pub(crate) struct ConnectorRequest {
    pub name: String,
    pub ctype: String,
    #[serde(default)]
    pub description: String,
    pub config: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ToggleRequest {
    pub enabled: bool,
}

/// Validate a connector request body; returns a parsed connector draft.
fn validate_request(user_id: &str, req: &ConnectorRequest) -> Result<Connector, String> {
    if req.name.trim().is_empty() {
        return Err("Connector name must not be empty".to_string());
    }
    match req.ctype.as_str() {
        TYPE_API => {
            let cfg: ApiConnectorConfig = serde_json::from_value(req.config.clone())
                .map_err(|e| format!("Invalid API connector config: {}", e))?;
            for tool in &cfg.tools {
                jcowork_connectors::api_executor::validate_api_tool(tool)
                    .map_err(|e| format!("Tool '{}': {}", tool.name, e))?;
            }
            Ok(serde_json::to_value(&cfg).map(|v| (req, v)).map_err(|e| e.to_string())
                .map(|(req, config)| build_connector(user_id, req, config))?)
        }
        TYPE_MCP => {
            let cfg: McpConfig = serde_json::from_value(req.config.clone())
                .map_err(|e| format!("Invalid MCP config: {}", e))?;
            cfg.validate().map_err(|msg| format!("Invalid MCP config: {}", msg))?;
            Ok(build_connector(user_id, req, req.config.clone()))
        }
        other => Err(format!("Unknown connector type: {}", other)),
    }
}

fn build_connector(user_id: &str, req: &ConnectorRequest, config: serde_json::Value) -> Connector {
    Connector {
        id: String::new(), // filled by create handler
        user_id: user_id.to_string(),
        name: req.name.trim().to_string(),
        ctype: req.ctype.clone(),
        description: req.description.clone(),
        config,
        tool_states: Default::default(),
        enabled: true,
        created_at: String::new(),
        updated_at: String::new(),
    }
}

pub(crate) async fn list_connectors(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
) -> impl IntoResponse {
    match state.connector_manager.store().list(&auth_user.user_id).await {
        Ok(connectors) => (StatusCode::OK, Json(serde_json::json!(connectors))),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ),
    }
}

pub(crate) async fn create_connector(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Json(req): Json<ConnectorRequest>,
) -> impl IntoResponse {
    let mut connector = match validate_request(&auth_user.user_id, &req) {
        Ok(c) => c,
        Err(msg) => return (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": msg}))),
    };
    connector.id = uuid::Uuid::new_v4().to_string();

    if let Err(e) = state.connector_manager.store().create(&connector).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        );
    }
    let _ = state.connector_manager.sync_registry().await;
    (StatusCode::OK, Json(serde_json::json!(connector)))
}

pub(crate) async fn get_connector(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.connector_manager.store().get(&auth_user.user_id, &id).await {
        Ok(connector) => (StatusCode::OK, Json(serde_json::json!(connector))),
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": e.to_string()})),
        ),
    }
}

pub(crate) async fn update_connector(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Path(id): Path<String>,
    Json(req): Json<ConnectorRequest>,
) -> impl IntoResponse {
    // Load first so preserved fields (enabled, tool_states) survive updates.
    let existing = match state.connector_manager.store().get(&auth_user.user_id, &id).await {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": e.to_string()})),
            )
        }
    };
    let mut connector = match validate_request(&auth_user.user_id, &req) {
        Ok(c) => c,
        Err(msg) => return (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": msg}))),
    };
    connector.id = id.clone();
    connector.enabled = existing.enabled;
    // Keep MCP tool-level states only when the connector stays the same type;
    // a type change invalidates previously discovered tool names.
    connector.tool_states = if existing.ctype == connector.ctype {
        existing.tool_states
    } else {
        Default::default()
    };

    if let Err(e) = state.connector_manager.store().update(&connector).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        );
    }
    state.connector_manager.invalidate_connector(&id);
    let _ = state.connector_manager.sync_registry().await;
    (StatusCode::OK, Json(serde_json::json!(connector)))
}

pub(crate) async fn delete_connector(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state
        .connector_manager
        .store()
        .delete(&auth_user.user_id, &id)
        .await
    {
        Ok(()) => {
            state.connector_manager.invalidate_connector(&id);
            let _ = state.connector_manager.sync_registry().await;
            (StatusCode::OK, Json(serde_json::json!({"status": "deleted"})))
        }
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": e.to_string()})),
        ),
    }
}

/// Toggle the connector-level enabled flag.
pub(crate) async fn toggle_connector(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Path(id): Path<String>,
    Json(req): Json<ToggleRequest>,
) -> impl IntoResponse {
    match state
        .connector_manager
        .store()
        .set_enabled(&auth_user.user_id, &id, req.enabled)
        .await
    {
        Ok(()) => {
            state.connector_manager.invalidate_connector(&id);
            let _ = state.connector_manager.sync_registry().await;
            (
                StatusCode::OK,
                Json(serde_json::json!({"enabled": req.enabled})),
            )
        }
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": e.to_string()})),
        ),
    }
}

/// Toggle the tool-level enabled flag (works for both API and MCP connectors).
pub(crate) async fn toggle_connector_tool(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Path((id, tool)): Path<(String, String)>,
    Json(req): Json<ToggleRequest>,
) -> impl IntoResponse {
    match state
        .connector_manager
        .store()
        .set_tool_enabled(&auth_user.user_id, &id, &tool, req.enabled)
        .await
    {
        Ok(()) => {
            let _ = state.connector_manager.sync_registry().await;
            (
                StatusCode::OK,
                Json(serde_json::json!({"tool": tool, "enabled": req.enabled})),
            )
        }
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": e.to_string()})),
        ),
    }
}

/// Test a connector configuration without saving it.
pub(crate) async fn test_connector(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Json(req): Json<ConnectorRequest>,
) -> impl IntoResponse {
    let mut connector = match validate_request(&auth_user.user_id, &req) {
        Ok(c) => c,
        Err(msg) => return (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": msg}))),
    };
    connector.id = "test".to_string();

    match state.connector_manager.test_connector(&connector).await {
        Ok(summary) => (StatusCode::OK, Json(serde_json::json!({"status": "ok", "message": summary}))),
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({"status": "error", "error": e.to_string()})),
        ),
    }
}

/// List the tools of a connector (name, description, parameters, enabled).
pub(crate) async fn list_connector_tools(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let connector = match state.connector_manager.store().get(&auth_user.user_id, &id).await {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": e.to_string()})),
            )
        }
    };
    match state.connector_manager.list_tools(&connector).await {
        Ok(tools) => (StatusCode::OK, Json(serde_json::json!(tools))),
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({"error": e.to_string()})),
        ),
    }
}
