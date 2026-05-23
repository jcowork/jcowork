//! REST API routes.

use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, post, put},
    Json, Router,
    middleware,
    extract::Request,
};
use axum::extract::{ws::WebSocketUpgrade, Path, Query};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::auth;
use crate::session::SessionManager;
use crate::ws;
use jcowork_cron::CronScheduler;
use jcowork_llm::LlmRouter;
use jcowork_logs::LogWriter;
use jcowork_memory::MemoryManager;
use jcowork_skills::{builtin_skills, SkillManager};
use jcowork_storage::UserStore;
use jcowork_tools::registry::ToolRegistry;

/// Shared application state.
#[derive(Clone)]
pub struct AppState {
    pub session_manager: Arc<SessionManager>,
    pub auth_config: auth::AuthConfig,
    pub llm_router: Arc<LlmRouter>,
    pub default_model: String,
    pub cron_scheduler: Arc<CronScheduler>,
    pub memory_manager: Arc<MemoryManager>,
    pub skill_manager: Arc<SkillManager>,
    pub tool_registry: Arc<ToolRegistry>,
    pub user_store: Arc<UserStore>,
    pub log_writer: Arc<LogWriter>,
}

// --- Request/Response types ---

#[derive(Debug, Deserialize)]
pub struct MemorySearchQuery {
    query: Option<String>,
    limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateMemoryRequest {
    pub content: Option<String>,
    pub category: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RegisterRequest {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Serialize)]
pub struct AuthResponse {
    pub token: String,
    pub user_id: String,
    pub username: String,
}

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Deserialize)]
pub struct ChatRequest {
    pub message: String,
    pub session_id: Option<String>,
    pub model: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ChatResponse {
    pub session_id: String,
    pub response: String,
}

#[derive(Debug, Serialize)]
pub struct SessionInfo {
    pub id: String,
    pub title: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Serialize)]
pub struct MemoryInfo {
    pub id: String,
    pub content: String,
    pub category: String,
}

#[derive(Debug, Serialize)]
pub struct SkillInfo {
    pub name: String,
    pub description: Option<String>,
    pub version: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthUser {
    pub user_id: String,
    pub username: String,
}

// --- Route handlers ---

pub fn build_router(state: AppState) -> Router {
    let auth_mw = axum::middleware::from_fn_with_state(state.clone(), auth_middleware);

    // Public routes (no auth required)
    let public = Router::new()
        .route("/api/auth/register", post(register))
        .route("/api/auth/login", post(login))
        .route("/api/health", get(health));

    // Protected routes (auth required)
    let protected = Router::new()
        .route("/api/chat", post(chat))
        .route("/api/sessions", get(list_sessions))
        .route("/api/skills", get(list_skills))
        .route("/api/skills", post(create_skill))
        .route("/api/skills/all", get(list_all_skills))
        .route("/api/skills/{id}/toggle", put(toggle_skill))
        .route("/api/memory", get(list_memories))
        .route("/api/memory/search", get(search_memories))
        .route("/api/memory/{id}", put(update_memory))
        .route("/api/memory/{id}", delete(delete_memory))
        .route("/api/reminders", get(list_reminders))
        .route("/api/reminders/{id}", delete(remove_reminder))
        .route("/api/cron-jobs", get(list_cron_jobs))
        .route("/api/cron-jobs/{id}", delete(remove_cron_job))
        .route("/api/providers", get(list_providers))
        .route("/api/agent-identity", get(get_agent_identity))
        .route("/api/agent-identity", put(set_agent_identity))
        .route("/api/ws", get(ws_upgrade))
        .layer(auth_mw);

    Router::new()
        .merge(public)
        .merge(protected)
        .with_state(state)
}

/// JWT authentication middleware.
/// Extracts user_id from the Bearer token and inserts it into request extensions.
async fn auth_middleware(
    State(state): State<AppState>,
    mut req: Request,
    next: middleware::Next,
) -> impl IntoResponse {
    // Try Authorization header first, then query param ?token=
    let token = req.headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|s| s.to_string())
        .or_else(|| {
            req.uri().query().and_then(|q| {
                q.split('&')
                    .find_map(|pair| {
                        let (k, v) = pair.split_once('=')?;
                        if k == "token" { Some(v.to_string()) } else { None }
                    })
            })
        });

    let token = match token {
        Some(t) => t,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "Missing authentication token"})),
            ).into_response();
        }
    };

    match auth::verify_token(&state.auth_config, &token) {
        Ok(claims) => {
            // Insert authenticated user into request extensions
            req.extensions_mut().insert(AuthUser {
                user_id: claims.sub,
                username: claims.username,
            });
            next.run(req).await
        }
        Err(e) => {
            (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": format!("Invalid token: {}", e)})),
            ).into_response()
        }
    }
}

async fn health() -> &'static str {
    "ok"
}

async fn register(
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

async fn login(
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

async fn chat(
    State(_state): State<AppState>,
    _auth: axum::Extension<AuthUser>,
    Json(req): Json<ChatRequest>,
) -> impl IntoResponse {
    // In full impl: route to UserActor, stream response back
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "session_id": req.session_id.unwrap_or_default(),
            "response": "Agent response placeholder",
        })),
    )
}

async fn list_sessions(
    _auth: axum::Extension<AuthUser>,
) -> impl IntoResponse {
    let sessions: Vec<SessionInfo> = Vec::new();
    (StatusCode::OK, Json(sessions))
}

async fn list_skills(
    _auth: axum::Extension<AuthUser>,
) -> impl IntoResponse {
    let skills: Vec<SkillInfo> = Vec::new();
    (StatusCode::OK, Json(skills))
}

async fn create_skill(
    _auth: axum::Extension<AuthUser>,
) -> impl IntoResponse {
    (StatusCode::OK, Json(serde_json::json!({"status": "created"})))
}

/// Unified skill entry returned by /api/skills/all
#[derive(Debug, Serialize)]
struct SkillEntry {
    id: String,
    name: String,
    description: String,
    content: String,
    source: String, // "builtin" or "user"
    version: i32,
    enabled: bool,
}

async fn list_all_skills(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
) -> impl IntoResponse {
    // Load enabled skill IDs from memory (category = "skill_enabled", content = skill id)
    let enabled_ids: std::collections::HashSet<String> = state
        .memory_manager
        .recall_all(&auth_user.user_id)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|e| e.category == "skill_enabled")
        .map(|e| e.content)
        .collect();

    let mut entries: Vec<SkillEntry> = Vec::new();

    // Built-in skills
    for s in builtin_skills() {
        entries.push(SkillEntry {
            id: s.id.to_string(),
            name: s.name.to_string(),
            description: s.description.to_string(),
            content: s.content.to_string(),
            source: "builtin".to_string(),
            version: 1,
            enabled: enabled_ids.contains(s.id),
        });
    }

    // User skills
    if let Ok(user_skills) = state.skill_manager.list(&auth_user.user_id).await {
        for s in user_skills {
            let enabled = enabled_ids.contains(&s.id);
            entries.push(SkillEntry {
                id: s.id.clone(),
                name: s.name,
                description: s.description.unwrap_or_default(),
                content: s.content,
                source: "user".to_string(),
                version: s.version,
                enabled,
            });
        }
    }

    (StatusCode::OK, Json(entries))
}

#[derive(Debug, Deserialize)]
struct ToggleSkillRequest {
    enabled: bool,
}

async fn toggle_skill(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Path(skill_id): Path<String>,
    Json(req): Json<ToggleSkillRequest>,
) -> impl IntoResponse {
    // Remove existing entry for this skill_id in skill_enabled category
    if let Ok(entries) = state.memory_manager.recall_all(&auth_user.user_id).await {
        for entry in entries.into_iter().filter(|e| e.category == "skill_enabled" && e.content == skill_id) {
            let _ = state.memory_manager.delete(&auth_user.user_id, &entry.id).await;
        }
    }
    if req.enabled {
        match state.memory_manager.save(&auth_user.user_id, &skill_id, "skill_enabled").await {
            Ok(_) => (StatusCode::OK, Json(serde_json::json!({ "status": "enabled" }))),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({ "error": e.to_string() }))),
        }
    } else {
        (StatusCode::OK, Json(serde_json::json!({ "status": "disabled" })))
    }
}

async fn list_memories(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
) -> impl IntoResponse {
    match state.memory_manager.recall_all(&auth_user.user_id).await {
        Ok(entries) => {
            let infos: Vec<MemoryInfo> = entries
                .into_iter()
                .map(|e| MemoryInfo {
                    id: e.id,
                    content: e.content,
                    category: e.category,
                })
                .collect();
            (StatusCode::OK, Json(infos))
        }
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, Json(Vec::<MemoryInfo>::new())),
    }
}

async fn search_memories(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Query(params): Query<MemorySearchQuery>,
) -> impl IntoResponse {
    let query = params.query.unwrap_or_default();
    let limit = params.limit.unwrap_or(10) as usize;
    match state.memory_manager.search(&auth_user.user_id, &query, limit).await {
        Ok(results) => {
            let infos: Vec<MemoryInfo> = results
                .into_iter()
                .map(|r| MemoryInfo {
                    id: r.id,
                    content: r.content,
                    category: r.category,
                })
                .collect();
            (StatusCode::OK, Json(infos))
        }
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, Json(Vec::<MemoryInfo>::new())),
    }
}

async fn list_reminders(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
) -> impl IntoResponse {
    let reminders = state.cron_scheduler.list_reminders(&auth_user.user_id).await;
    (StatusCode::OK, Json(reminders))
}

async fn remove_reminder(
    State(state): State<AppState>,
    _auth: axum::Extension<AuthUser>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.cron_scheduler.remove_reminder(&id).await {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({"status": "removed"}))),
        Err(e) => (StatusCode::NOT_FOUND, Json(serde_json::json!({"error": e.to_string()}))),
    }
}

async fn list_cron_jobs(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
) -> impl IntoResponse {
    let jobs = state.cron_scheduler.list_cron_jobs(&auth_user.user_id).await;
    (StatusCode::OK, Json(jobs))
}

async fn remove_cron_job(
    State(state): State<AppState>,
    _auth: axum::Extension<AuthUser>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.cron_scheduler.remove_cron_job(&id).await {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({"status": "removed"}))),
        Err(e) => (StatusCode::NOT_FOUND, Json(serde_json::json!({"error": e.to_string()}))),
    }
}

async fn update_memory(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Path(id): Path<String>,
    Json(req): Json<UpdateMemoryRequest>,
) -> impl IntoResponse {
    match state.memory_manager.update(
        &auth_user.user_id,
        &id,
        req.content.as_deref(),
        req.category.as_deref(),
    ).await {
        Ok(entry) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "id": entry.id,
                "content": entry.content,
                "category": entry.category,
                "updated_at": entry.updated_at,
            })),
        ),
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": e.to_string()})),
        ),
    }
}

async fn delete_memory(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.memory_manager.delete(&auth_user.user_id, &id).await {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({"status": "deleted"}))),
        Err(e) => (StatusCode::NOT_FOUND, Json(serde_json::json!({"error": e.to_string()}))),
    }
}

async fn list_providers(
    State(state): State<AppState>,
    _auth: axum::Extension<AuthUser>,
) -> impl IntoResponse {
    let providers = state.llm_router.providers_info();
    let default_model = &state.default_model;
    (StatusCode::OK, Json(serde_json::json!({
        "providers": providers,
        "default_model": default_model,
    })))
}

async fn get_agent_identity(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
) -> impl IntoResponse {
    match state.memory_manager.search(&auth_user.user_id, "agent_identity", 1).await {
        Ok(results) => {
            let identity = results.into_iter().next().map(|r| r.content).unwrap_or_default();
            (StatusCode::OK, Json(serde_json::json!({ "identity": identity })))
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "identity": "", "error": e.to_string() })),
        ),
    }
}

#[derive(Debug, Deserialize)]
struct SetAgentIdentityRequest {
    pub identity: String,
}

async fn set_agent_identity(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Json(req): Json<SetAgentIdentityRequest>,
) -> impl IntoResponse {
    // Delete existing agent_identity entries first
    if let Ok(entries) = state.memory_manager.recall_all(&auth_user.user_id).await {
        for entry in entries.into_iter().filter(|e| e.category == "agent_identity") {
            let _ = state.memory_manager.delete(&auth_user.user_id, &entry.id).await;
        }
    }
    // Save new identity (empty string = reset to default)
    let identity = req.identity.trim().to_string();
    if identity.is_empty() {
        return (StatusCode::OK, Json(serde_json::json!({ "status": "reset" })));
    }
    match state.memory_manager.save(&auth_user.user_id, &identity, "agent_identity").await {
        Ok(_) => (StatusCode::OK, Json(serde_json::json!({ "status": "saved" }))),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        ),
    }
}

async fn ws_upgrade(
    ws: WebSocketUpgrade,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    let user_id = auth_user.user_id;
    let default_model = state.default_model.clone();
    let tool_registry = state.tool_registry.clone();
    let cron_scheduler = state.cron_scheduler.clone();
    let log_writer = state.log_writer.clone();
    let memory_manager = state.memory_manager.clone();
    let skill_manager = state.skill_manager.clone();
    ws.on_upgrade(move |socket| {
        ws::ws_handler(socket, user_id, state.session_manager, state.llm_router, default_model, tool_registry, cron_scheduler, log_writer, memory_manager, skill_manager)
    })
}
