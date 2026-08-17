//! REST API routes.

use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, post, put},
    Json, Router,
    middleware,
    extract::Request,
    extract::DefaultBodyLimit,
};
use axum::extract::{ws::WebSocketUpgrade, Multipart, Path, Query};
use axum::http::header;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::RwLock;
use tower_http::services::{ServeDir, ServeFile};

use crate::auth;
use crate::session::SessionManager;
use crate::ws;
use dashmap::DashMap;
use jcowork_cron::CronScheduler;
use jcowork_feishu::client::FeishuClient;
use jcowork_llm::{LlmRouter, ProviderEntry};
use jcowork_logs::LogWriter;
use jcowork_memory::MemoryManager;
use jcowork_skills::{builtin_skills, SkillManager};
use jcowork_storage::{FeishuConfigStore, FileStore, UserStore, WorkspaceIndex};
use jcowork_tools::registry::ToolRegistry;

/// Shared application state.
#[derive(Clone)]
pub struct AppState {
    pub session_manager: Arc<SessionManager>,
    pub auth_config: auth::AuthConfig,
    pub llm_router: Arc<RwLock<LlmRouter>>,
    pub default_model: String,
    pub cron_scheduler: Arc<CronScheduler>,
    pub memory_manager: Arc<MemoryManager>,
    pub skill_manager: Arc<SkillManager>,
    pub tool_registry: Arc<ToolRegistry>,
    pub user_store: Arc<UserStore>,
    pub log_writer: Arc<LogWriter>,
    /// Per-user Feishu config store (database-backed).
    pub feishu_config_store: Arc<FeishuConfigStore>,
    /// Cache of FeishuClient instances keyed by app_id.
    pub feishu_client_cache: Arc<DashMap<String, Arc<FeishuClient>>>,
    /// Data directory for per-user workspaces.
    pub data_dir: String,
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
        .route("/api/health", get(health))
        .route("/api/feishu/event", post(crate::feishu::feishu_event_handler));

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
        .route("/api/providers/entries", get(list_provider_entries))
        .route("/api/providers", post(save_providers))
        .route("/api/agent-identity", get(get_agent_identity))
        .route("/api/agent-identity", put(set_agent_identity))
        .route("/api/feishu/config", get(get_feishu_config))
        .route("/api/feishu/config", put(set_feishu_config))
        .route("/api/feishu/config", delete(delete_feishu_config))
        .route("/api/workspace/files", get(list_workspace_files))
        .route("/api/workspace/download", get(download_workspace_file))
        .route("/api/workspace/upload-pdf", post(upload_pdf))
        .route("/api/workspace/parse-pdf", post(parse_workspace_pdf))
        .route("/api/workspace/upload", post(upload_file))
        .route("/api/workspace/mkdir", post(create_directory))
        .route("/api/workspace/files-recursive", get(list_workspace_files_recursive))
        .route("/api/workspace/delete", post(delete_workspace_file))
        .route("/api/workspace/move", post(move_workspace_path))
        .route("/api/workspace/index/search", get(search_workspace_index))
        .route("/api/workspace/index/list", get(list_workspace_index))
        .route("/api/workspace/index/content", get(get_indexed_content))
        .route("/api/workspace/index/reindex", post(reindex_workspace_dir))
        .route("/api/workspace/vector/search", get(vector_search_chunks))
        .route("/api/workspace/doc/chunks", get(get_document_chunks))
        .route("/api/workspace/doc/image", get(get_document_image))
        .route("/api/workspace/excel-db", get(get_excel_db_content))
        .route("/api/fetch-url", post(fetch_url))
        .route("/api/ws", get(ws_upgrade))
        .layer(auth_mw)
        // Allow up to 50MB for file uploads (default axum limit is only 2MB)
        .layer(DefaultBodyLimit::max(50 * 1024 * 1024));

    Router::new()
        .merge(public)
        .merge(protected)
        .fallback_service(static_files_handler())
        .with_state(state)
}

/// Serve static frontend files with proper cache headers.
/// - index.html: no-cache (always revalidate, so updates take effect immediately)
/// - /assets/*: long cache (content-hashed filenames)
/// - SPA fallback: serve index.html for unknown routes
fn static_files_handler() -> Router {
    // Determine web dist directory
    let web_dir = std::env::var("JCWORK_WEB_DIR")
        .unwrap_or_else(|_| {
            let candidates = [
                "web/dist",
                "/opt/jcowork/web/dist",
            ];
            for c in &candidates {
                if std::path::Path::new(c).exists() {
                    return c.to_string();
                }
            }
            "web/dist".to_string()
        });

    let index_html = format!("{}/index.html", web_dir);
    let assets_dir = format!("{}/assets", web_dir);

    // Assets with content hashes can be cached forever
    let assets_service = ServeDir::new(&assets_dir);

    // Fallback: serve from web_dir, with index.html as not-found fallback (SPA)
    let fallback_service = ServeDir::new(&web_dir)
        .not_found_service(ServeFile::new(&index_html));

    Router::new()
        // /assets/* → long cache (content-hashed filenames)
        .nest_service("/assets", assets_service)
        // / and /index.html → no-cache (always revalidate on update)
        .route("/index.html", get({
            let path = index_html.clone();
            move || {
                let path = path.clone();
                async move { serve_index_no_cache(path).await }
            }
        }))
        .route("/", get(move || {
            async move { serve_index_no_cache(index_html).await }
        }))
        // Everything else → serve from dist, fallback to index.html (SPA routing)
        .fallback_service(fallback_service)
}

/// Serve index.html with no-cache headers so browsers always fetch the latest version.
async fn serve_index_no_cache(path: String) -> impl IntoResponse {
    let content = tokio::fs::read(&path).await.unwrap_or_default();
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache, no-store, must-revalidate"),
            (header::PRAGMA, "no-cache"),
        ],
        content,
    )
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

    // Built-in skills (hidden ones stay functional but are not shown in the UI)
    for s in builtin_skills().iter().filter(|s| !s.hidden) {
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
    let router = state.llm_router.read().unwrap();
    let providers = router.providers_info();
    let default_model = &state.default_model;
    (StatusCode::OK, Json(serde_json::json!({
        "providers": providers,
        "default_model": default_model,
    })))
}

/// GET /api/providers/entries — returns full provider entries (with api_key masked).
async fn list_provider_entries(
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
struct SaveProvidersRequest {
    pub entries: Vec<ProviderEntry>,
}

/// POST /api/providers — save all provider entries and rebuild the router.
async fn save_providers(
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
    let data_dir = state.data_dir.clone();
    let llm_router = state.llm_router.clone();
    ws.on_upgrade(move |socket| {
        ws::ws_handler(socket, user_id, state.session_manager, llm_router, default_model, tool_registry, cron_scheduler, log_writer, memory_manager, skill_manager, data_dir)
    })
}

// --- Feishu Config API ---

#[derive(Debug, Serialize)]
#[allow(dead_code)]
struct FeishuConfigResponse {
    app_id: String,
    app_secret_masked: String,
    verification_token: String,
    encrypt_key: String,
    is_configured: bool,
}

#[derive(Debug, Deserialize)]
struct FeishuConfigRequest {
    app_id: String,
    app_secret: String,
    verification_token: String,
    encrypt_key: Option<String>,
}

async fn get_feishu_config(
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

async fn set_feishu_config(
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

async fn delete_feishu_config(
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

/// Mask a secret string, showing only the last 4 characters.
fn mask_secret(secret: &str) -> String {
    if secret.len() <= 4 {
        "****".to_string()
    } else {
        format!("{}{}", "*".repeat(secret.len() - 4), &secret[secret.len() - 4..])
    }
}

// --- Workspace Files API ---

#[derive(Debug, Deserialize)]
struct WorkspaceFilesQuery {
    path: Option<String>,
}

async fn list_workspace_files(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Query(params): Query<WorkspaceFilesQuery>,
) -> impl IntoResponse {
    let workspace_root = format!("{}/{}/workspace", state.data_dir, auth_user.user_id);
    // Ensure workspace exists
    let _ = tokio::fs::create_dir_all(&workspace_root).await;

    let store = FileStore::new(&workspace_root);
    let path = params.path.unwrap_or_else(|| ".".to_string());

    match store.list_dir_detailed(&path).await {
        Ok(entries) => {
            let items: Vec<serde_json::Value> = entries
                .iter()
                .filter_map(|entry| {
                    let parts: Vec<&str> = entry.splitn(2, '\t').collect();
                    if parts.len() == 2 {
                        Some(serde_json::json!({
                            "name": parts[0],
                            "type": parts[1],
                        }))
                    } else {
                        None
                    }
                })
                .collect();
            (StatusCode::OK, Json(serde_json::json!(items))).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        ).into_response(),
    }
}

#[derive(Debug, Deserialize)]
struct DownloadFileQuery {
    path: String,
}

// --- PDF Upload API ---

/// Inline Python script for PDF text extraction using pdftext.
/// Must match the script in jcowork-tools/pdf_parse.rs.
const PDF_PARSE_SCRIPT: &str = r#"
import sys
import os

path = sys.argv[1]

if not os.path.isfile(path):
    print(f"Error: path does not exist: {path}", file=sys.stderr)
    sys.exit(1)

from pdftext.extraction import plain_text_output

try:
    text = plain_text_output(path)
    print(text)
except Exception as e:
    print(f"Error parsing PDF: {e}", file=sys.stderr)
    sys.exit(1)
"#;

async fn upload_pdf(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    mut multipart: Multipart,
) -> impl IntoResponse {
    let workspace_root = format!("{}/{}/workspace", state.data_dir, auth_user.user_id);
    let _ = tokio::fs::create_dir_all(&workspace_root).await;
    let uploads_dir = format!("{}/uploads", workspace_root);
    let _ = tokio::fs::create_dir_all(&uploads_dir).await;

    let mut result_files: Vec<serde_json::Value> = Vec::new();

    while let Ok(Some(field)) = multipart.next_field().await {
        let filename = field
            .file_name()
            .unwrap_or("uploaded.pdf")
            .to_string();
        // Sanitize filename
        let safe_name = filename
            .replace("..", "")
            .replace('/', "_")
            .replace('\\', "_");
        let file_path = format!("{}/{}", uploads_dir, safe_name);
        let relative_path = format!("uploads/{}", safe_name);

        let data = match field.bytes().await {
            Ok(d) => d,
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({ "error": format!("Failed to read file: {}", e) })),
                ).into_response();
            }
        };

        // Save PDF to workspace
        if let Err(e) = tokio::fs::write(&file_path, &data).await {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("Failed to save file: {}", e) })),
            ).into_response();
        }

        // Parse PDF using pdftext
        let python_bin = {
            let home = std::env::var("HOME")
                .or_else(|_| std::env::var("USERPROFILE"))
                .unwrap_or_else(|_| "/tmp".to_string());
            if cfg!(windows) {
                format!("{}\\.jcowork\\venv\\Scripts\\python.exe", home)
            } else {
                format!("{}/.jcowork/venv/bin/python", home)
            }
        };

        let parsed_text = if std::path::Path::new(&python_bin).exists() {
            match tokio::time::timeout(
                std::time::Duration::from_secs(120),
                tokio::process::Command::new(&python_bin)
                    .arg("-c")
                    .arg(PDF_PARSE_SCRIPT)
                    .arg(&file_path)
                    .output(),
            ).await {
                Ok(Ok(output)) if output.status.success() => {
                    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
                    // Truncate if too large
                    if text.len() > 200 * 1024 {
                        text.truncate(200 * 1024);
                        text.push_str("\n\n[... OUTPUT TRUNCATED: document exceeds 200KB ...]");
                    }
                    text
                }
                Ok(Ok(output)) => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    format!("[PDF parse error: {}]", stderr)
                }
                Ok(Err(e)) => format!("[Failed to run PDF parser: {}]", e),
                Err(_) => "[PDF parsing timed out after 120s]".to_string(),
            }
        } else {
            "[Python venv not found. Run scripts/setup-python.sh first.]".to_string()
        };

        result_files.push(serde_json::json!({
            "filename": safe_name,
            "path": relative_path,
            "size": data.len(),
            "text": parsed_text,
        }));
    }

    if result_files.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "No files uploaded" })),
        ).into_response();
    }

    (StatusCode::OK, Json(serde_json::json!({ "files": result_files }))).into_response()
}

// --- Parse workspace PDF API ---

#[derive(Debug, Deserialize)]
struct ParsePdfRequest {
    path: String,
}

/// Parse a PDF file that already exists in the user's workspace.
/// Returns the extracted text content.
async fn parse_workspace_pdf(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Json(body): Json<ParsePdfRequest>,
) -> impl IntoResponse {
    let workspace_root = format!("{}/{}/workspace", state.data_dir, auth_user.user_id);
    let store = FileStore::new(&workspace_root);

    // Validate path is within workspace
    let full_path = match store.validate_path_public(&body.path) {
        Ok(p) => p,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": e.to_string() })),
            ).into_response();
        }
    };

    // Check file exists
    if !full_path.is_file() {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": format!("File not found: {}", body.path) })),
        ).into_response();
    }

    // Parse PDF using pdftext
    let python_bin = {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| "/tmp".to_string());
        if cfg!(windows) {
            format!("{}\\.jcowork\\venv\\Scripts\\python.exe", home)
        } else {
            format!("{}/.jcowork/venv/bin/python", home)
        }
    };

    let parsed_text = if std::path::Path::new(&python_bin).exists() {
        match tokio::time::timeout(
            std::time::Duration::from_secs(120),
            tokio::process::Command::new(&python_bin)
                .arg("-c")
                .arg(PDF_PARSE_SCRIPT)
                .arg(&full_path)
                .output(),
        ).await {
            Ok(Ok(output)) if output.status.success() => {
                let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
                if text.len() > 200 * 1024 {
                    text.truncate(200 * 1024);
                    text.push_str("\n\n[... OUTPUT TRUNCATED: document exceeds 200KB ...]");
                }
                text
            }
            Ok(Ok(output)) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                format!("[PDF parse error: {}]", stderr)
            }
            Ok(Err(e)) => format!("[Failed to run PDF parser: {}]", e),
            Err(_) => "[PDF parsing timed out after 120s]".to_string(),
        }
    } else {
        "[Python venv not found. Run scripts/setup-python.sh first.]".to_string()
    };

    let filename = body.path.rsplit('/').next().unwrap_or(&body.path);
    (StatusCode::OK, Json(serde_json::json!({
        "filename": filename,
        "path": body.path,
        "text": parsed_text,
    }))).into_response()
}

// --- URL Fetch API ---

#[derive(Debug, Deserialize)]
struct FetchUrlRequest {
    url: String,
}

/// Convert HTML to plain text by stripping tags and decoding entities.
/// Good enough for LLM context — no external HTML parser dependency needed.
fn html_to_text(html: &str) -> String {
    let mut text = html.to_string();

    // Remove script and style blocks (including content)
    for tag in ["script", "style", "noscript", "head"] {
        let open = format!("<{}", tag);
        let close = format!("</{}>", tag);
        while let Some(start) = text.to_lowercase().find(&open) {
            if let Some(end) = text.to_lowercase()[start..].find(&close) {
                let abs_end = start + end + close.len();
                text.replace_range(start..abs_end, " ");
            } else {
                // No closing tag — remove to end of open tag
                if let Some(gt) = text[start..].find('>') {
                    text.replace_range(start..start + gt + 1, " ");
                } else {
                    break;
                }
            }
        }
    }

    // Replace <br>, <p>, <div>, <li> tags with newlines
    for tag in ["<br", "<br/", "<br /", "<p", "</p>", "<div", "</div>", "<li", "</li>", "<h1", "<h2", "<h3", "<h4", "<h5", "<h6", "</h1>", "</h2>", "</h3>", "</h4>", "</h5>", "</h6>", "<tr", "</tr>"] {
        let replacement = if tag.starts_with("</") { "\n" } else { "\n" };
        text = text.replace(tag, replacement);
    }

    // Strip all remaining HTML tags
    let mut result = String::with_capacity(text.len());
    let mut in_tag = false;
    for ch in text.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => result.push(ch),
            _ => {}
        }
    }

    // Decode common HTML entities
    let result = result
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
        .replace("&#x27;", "'")
        .replace("&mdash;", "—")
        .replace("&ndash;", "–")
        .replace("&hellip;", "…")
        .replace("&copy;", "©")
        .replace("&reg;", "®");

    // Collapse multiple whitespace/newlines
    let mut cleaned = String::with_capacity(result.len());
    let mut prev_was_space = false;
    for line in result.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if !prev_was_space {
                cleaned.push('\n');
                prev_was_space = true;
            }
        } else {
            // Collapse internal whitespace
            let collapsed: String = trimmed.split_whitespace().collect::<Vec<_>>().join(" ");
            cleaned.push_str(&collapsed);
            cleaned.push('\n');
            prev_was_space = false;
        }
    }

    // Truncate to 100KB to avoid token overflow
    let mut final_text = cleaned.trim().to_string();
    if final_text.len() > 100 * 1024 {
        final_text.truncate(100 * 1024);
        final_text.push_str("\n\n[... CONTENT TRUNCATED: page exceeds 100KB ...]");
    }
    final_text
}

/// Extract the page title from HTML.
fn extract_title(html: &str) -> String {
    let lower = html.to_lowercase();
    if let Some(start) = lower.find("<title") {
        if let Some(gt) = lower[start..].find('>') {
            let content_start = start + gt + 1;
            if let Some(end_tag) = lower[content_start..].find("</title>") {
                return html[content_start..content_start + end_tag].trim().to_string();
            }
        }
    }
    "Untitled".to_string()
}

async fn fetch_url(
    axum::Extension(_auth_user): axum::Extension<AuthUser>,
    Json(req): Json<FetchUrlRequest>,
) -> impl IntoResponse {
    let url = req.url.trim();

    // Basic URL validation
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "URL must start with http:// or https://" })),
        ).into_response();
    }

    // Fetch the page
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::limited(5))
        .user_agent("Mozilla/5.0 (compatible; JcoworkBot/1.0)")
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("Failed to create HTTP client: {}", e) })),
            ).into_response();
        }
    };

    let resp = match client.get(url).send().await {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({ "error": format!("Failed to fetch URL: {}", e) })),
            ).into_response();
        }
    };

    let status = resp.status();
    if !status.is_success() {
        return (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({ "error": format!("HTTP {} from {}", status, url) })),
        ).into_response();
    }

    let html = match resp.text().await {
        Ok(t) => t,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({ "error": format!("Failed to read response: {}", e) })),
            ).into_response();
        }
    };

    let title = extract_title(&html);
    let text = html_to_text(&html);

    Json(serde_json::json!({
        "url": url,
        "title": title,
        "text": text,
    })).into_response()
}

// --- Workspace Files Recursive API ---

#[derive(Debug, Deserialize)]
struct WorkspaceFilesRecursiveQuery {
    path: Option<String>,
}

async fn list_workspace_files_recursive(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Query(params): Query<WorkspaceFilesRecursiveQuery>,
) -> impl IntoResponse {
    let workspace_root = format!("{}/{}/workspace", state.data_dir, auth_user.user_id);
    let _ = tokio::fs::create_dir_all(&workspace_root).await;

    let store = FileStore::new(&workspace_root);
    let path = params.path.unwrap_or_else(|| ".".to_string());

    match store.list_dir_recursive(&path).await {
        Ok(files) => {
            (StatusCode::OK, Json(serde_json::json!(files))).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        ).into_response(),
    }
}

async fn download_workspace_file(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Query(params): Query<DownloadFileQuery>,
) -> impl IntoResponse {
    let workspace_root = format!("{}/{}/workspace", state.data_dir, auth_user.user_id);
    let store = FileStore::new(&workspace_root);

    match store.read_file(&params.path).await {
        Ok(content) => {
            // Determine content type from extension
            let content_type = if params.path.ends_with(".html") || params.path.ends_with(".htm") {
                "text/html; charset=utf-8"
            } else if params.path.ends_with(".css") {
                "text/css; charset=utf-8"
            } else if params.path.ends_with(".js") || params.path.ends_with(".mjs") {
                "application/javascript; charset=utf-8"
            } else if params.path.ends_with(".json") {
                "application/json; charset=utf-8"
            } else if params.path.ends_with(".svg") {
                "image/svg+xml"
            } else if params.path.ends_with(".png") || params.path.ends_with(".jpg")
                || params.path.ends_with(".jpeg") || params.path.ends_with(".gif")
                || params.path.ends_with(".webp") || params.path.ends_with(".ico")
            {
                // For binary images, we'd need a different approach; return as download
                "application/octet-stream"
            } else {
                "text/plain; charset=utf-8"
            };

            let filename = params.path.rsplit('/').next().unwrap_or("file");
            (
                StatusCode::OK,
                [
                    (axum::http::header::CONTENT_TYPE, content_type.to_string()),
                    (axum::http::header::CONTENT_DISPOSITION, format!("inline; filename=\"{}\"", filename)),
                ],
                content,
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

// --- Create Directory API ---

#[derive(Debug, Deserialize)]
struct MkdirRequest {
    path: String,
}

async fn create_directory(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Json(body): Json<MkdirRequest>,
) -> impl IntoResponse {
    let workspace_root = format!("{}/{}/workspace", state.data_dir, auth_user.user_id);
    let _ = tokio::fs::create_dir_all(&workspace_root).await;
    let store = FileStore::new(&workspace_root);

    // Validate path is within workspace
    match store.validate_path_public(&body.path) {
        Ok(full_path) => {
            match tokio::fs::create_dir_all(&full_path).await {
                Ok(_) => (StatusCode::OK, Json(serde_json::json!({ "ok": true, "path": body.path }))).into_response(),
                Err(e) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({ "error": format!("Failed to create directory: {}", e) })),
                ).into_response(),
            }
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e.to_string() })),
        ).into_response(),
    }
}

// --- General File Upload API ---

/// Allowed file extensions for upload.
const ALLOWED_UPLOAD_EXTENSIONS: &[&str] = &["pdf", "md", "html", "htm", "xlsx", "xls", "docx", "doc"];

async fn upload_file(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    mut multipart: Multipart,
) -> impl IntoResponse {
    let workspace_root = format!("{}/{}/workspace", state.data_dir, auth_user.user_id);
    let _ = tokio::fs::create_dir_all(&workspace_root).await;

    // Optional target directory (relative to workspace root)
    let mut target_dir: Option<String> = None;
    let mut uploaded_files: Vec<serde_json::Value> = Vec::new();

    // First pass: collect all fields
    let mut fields_data: Vec<(String, String, Vec<u8>)> = Vec::new();

    while let Ok(Some(field)) = multipart.next_field().await {
        let field_name = field.name().unwrap_or("").to_string();
        let filename = field.file_name().unwrap_or("").to_string();
        let data = match field.bytes().await {
            Ok(d) => d.to_vec(),
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({ "error": format!("Failed to read file: {}", e) })),
                ).into_response();
            }
        };
        fields_data.push((field_name, filename, data));
    }

    // Extract target_dir from form fields
    for (field_name, filename, data) in &fields_data {
        if field_name == "path" && filename.is_empty() {
            // This is the target directory field; value is in data bytes
            target_dir = Some(String::from_utf8_lossy(data).trim().to_string());
        }
    }

    // Process file fields
    for (field_name, filename, data) in &fields_data {
        if field_name == "path" {
            // Skip the directory path field
            continue;
        }

        if filename.is_empty() {
            continue;
        }

        // Check file extension
        let ext = filename.rsplit('.').next().unwrap_or("").to_lowercase();
        if !ALLOWED_UPLOAD_EXTENSIONS.contains(&ext.as_str()) {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": format!("File type '{}' not allowed. Allowed types: pdf, md, html, xlsx, xls, docx, doc", ext)
                })),
            ).into_response();
        }

        // Sanitize filename
        let safe_name = filename
            .replace("..", "")
            .replace('/', "_")
            .replace('\\', "_");

        // Determine target directory
        let dest_dir = if let Some(ref dir) = target_dir {
            if dir.is_empty() || dir == "." {
                workspace_root.clone()
            } else {
                format!("{}/{}", workspace_root, dir)
            }
        } else {
            workspace_root.clone()
        };

        // Ensure target directory exists
        if let Err(e) = tokio::fs::create_dir_all(&dest_dir).await {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("Failed to create directory: {}", e) })),
            ).into_response();
        }

        let file_path = format!("{}/{}", dest_dir, safe_name);
        let relative_path = if let Some(ref dir) = target_dir {
            if dir.is_empty() || dir == "." {
                safe_name.clone()
            } else {
                format!("{}/{}", dir, safe_name)
            }
        } else {
            safe_name.clone()
        };

        // Save file
        if let Err(e) = tokio::fs::write(&file_path, data).await {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("Failed to save file: {}", e) })),
            ).into_response();
        }

        // Index the uploaded file
        let mut index_error: Option<String> = None;
        match WorkspaceIndex::cached(&state.data_dir, &auth_user.user_id).await {
            Ok(index) => {
                if let Err(e) = index.add_document(&relative_path, &workspace_root).await {
                    let err_msg = format!("Failed to index {}: {}", relative_path, e);
                    tracing::error!(file = %relative_path, err = %e, "Failed to index uploaded file");
                    index_error = Some(err_msg);
                }
            }
            Err(e) => {
                let err_msg = format!("Failed to open index database: {}", e);
                tracing::error!(user_id = %auth_user.user_id, err = %e, "Failed to initialize workspace index");
                index_error = Some(err_msg);
            }
        }

        uploaded_files.push(serde_json::json!({
            "filename": safe_name,
            "path": relative_path,
            "size": data.len(),
            "indexed": index_error.is_none(),
            "index_error": index_error,
        }));
    }

    if uploaded_files.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "No files uploaded" })),
        ).into_response();
    }

    (StatusCode::OK, Json(serde_json::json!({ "files": uploaded_files }))).into_response()
}

// --- Delete Workspace File/Directory API ---

#[derive(Debug, Deserialize)]
struct DeleteWorkspaceRequest {
    path: String,
}

async fn delete_workspace_file(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Json(body): Json<DeleteWorkspaceRequest>,
) -> impl IntoResponse {
    let workspace_root = format!("{}/{}/workspace", state.data_dir, auth_user.user_id);
    let store = FileStore::new(&workspace_root);

    // Validate path
    if let Err(e) = store.validate_path_public(&body.path) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": format!("Invalid path: {}", e) })),
        ).into_response();
    }

    let full_path = format!("{}/{}", workspace_root, body.path);

    // Check if it's a directory or file
    let metadata = match tokio::fs::metadata(&full_path).await {
        Ok(m) => m,
        Err(e) => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": format!("Path not found: {}", e) })),
            ).into_response();
        }
    };

    // Delete from filesystem
    let result = if metadata.is_dir() {
        tokio::fs::remove_dir_all(&full_path).await
    } else {
        tokio::fs::remove_file(&full_path).await
    };

    if let Err(e) = result {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("Failed to delete: {}", e) })),
        ).into_response();
    }

    // Update index
    if let Ok(index) = WorkspaceIndex::cached(&state.data_dir, &auth_user.user_id).await {
        if metadata.is_dir() {
            let _ = index.remove_directory(&body.path).await;
        } else {
            let _ = index.remove_file(&body.path).await;
        }
    }

    (StatusCode::OK, Json(serde_json::json!({ "deleted": body.path }))).into_response()
}

// --- Move/Rename Workspace File/Directory API ---

#[derive(Debug, Deserialize)]
struct MoveWorkspaceRequest {
    from: String,
    to: String,
}

async fn move_workspace_path(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Json(body): Json<MoveWorkspaceRequest>,
) -> impl IntoResponse {
    let workspace_root = format!("{}/{}/workspace", state.data_dir, auth_user.user_id);
    let store = FileStore::new(&workspace_root);

    // Validate paths
    if let Err(e) = store.validate_path_public(&body.from) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": format!("Invalid source path: {}", e) })),
        ).into_response();
    }
    if let Err(e) = store.validate_path_public(&body.to) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": format!("Invalid destination path: {}", e) })),
        ).into_response();
    }

    let from_full = format!("{}/{}", workspace_root, body.from);
    let to_full = format!("{}/{}", workspace_root, body.to);

    // Ensure destination parent exists
    if let Some(parent) = std::path::Path::new(&to_full).parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }

    // Move on filesystem
    if let Err(e) = tokio::fs::rename(&from_full, &to_full).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("Failed to move: {}", e) })),
        ).into_response();
    }

    // Update index
    if let Ok(index) = WorkspaceIndex::cached(&state.data_dir, &auth_user.user_id).await {
        if let Err(e) = index.move_path(&body.from, &body.to).await {
            tracing::warn!(from = %body.from, to = %body.to, err = %e, "Failed to update index on move");
        }

        // If destination is a new file that wasn't previously indexed, add it
        if let Ok(count) = index.count().await {
            // If the moved path was not in index (count unchanged), try to add it
            // This handles the case where a non-indexed file is moved into an indexed location
            let _ = count; // just to suppress warning
        }
    }

    (StatusCode::OK, Json(serde_json::json!({ "moved": { "from": body.from, "to": body.to } }))).into_response()
}

// --- Workspace Index Search API ---

#[derive(Debug, Deserialize)]
struct SearchIndexQuery {
    q: String,
    #[serde(default = "default_search_limit")]
    limit: u32,
}

fn default_search_limit() -> u32 {
    10
}

async fn search_workspace_index(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Query(query): Query<SearchIndexQuery>,
) -> impl IntoResponse {
    let index = match WorkspaceIndex::cached(&state.data_dir, &auth_user.user_id).await {
        Ok(idx) => idx,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("Failed to open index: {}", e) })),
            ).into_response();
        }
    };

    match index.search(&query.q, query.limit).await {
        Ok(docs) => {
            (StatusCode::OK, Json(serde_json::json!({ "results": docs, "total": docs.len() }))).into_response()
        }
        Err(e) => {
            // FTS query syntax error - return empty results with error message
            (
                StatusCode::OK,
                Json(serde_json::json!({ "results": [], "total": 0, "error": format!("Search error: {}", e) })),
            ).into_response()
        }
    }
}

// --- Workspace Index List API ---

#[derive(Debug, Deserialize)]
struct ListIndexQuery {
    dir: Option<String>,
}

async fn list_workspace_index(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Query(query): Query<ListIndexQuery>,
) -> impl IntoResponse {
    let index = match WorkspaceIndex::cached(&state.data_dir, &auth_user.user_id).await {
        Ok(idx) => idx,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("Failed to open index: {}", e) })),
            ).into_response();
        }
    };

    let result = if let Some(ref dir) = query.dir {
        index.list_by_directory(dir).await
    } else {
        index.list_all(None).await
    };

    match result {
        Ok(docs) => {
            (StatusCode::OK, Json(serde_json::json!({ "documents": docs, "total": docs.len() }))).into_response()
        }
        Err(e) => {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("Failed to list: {}", e) })),
            ).into_response()
        }
    }
}

// --- Get Indexed Document Content API ---

#[derive(Debug, Deserialize)]
struct ContentIndexQuery {
    path: String,
    /// 0-based character offset for paginated preview (requires `limit`).
    offset: Option<i64>,
    /// Max characters to return per page.
    limit: Option<i64>,
}

async fn get_indexed_content(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Query(query): Query<ContentIndexQuery>,
) -> impl IntoResponse {
    let index = match WorkspaceIndex::cached(&state.data_dir, &auth_user.user_id).await {
        Ok(idx) => idx,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("Failed to open index: {}", e) })),
            ).into_response();
        }
    };

    // Paginated mode: return a character slice plus metadata for "load more".
    if let Some(limit) = query.limit {
        let offset = query.offset.unwrap_or(0).max(0);
        let limit = limit.clamp(1, 200_000);
        return match index.get_content_slice(&query.path, offset, limit).await {
            Ok(Some((content, total_len))) => {
                let next_offset = offset + content.chars().count() as i64;
                (StatusCode::OK, Json(serde_json::json!({
                    "path": query.path,
                    "content": content,
                    "total_len": total_len,
                    "next_offset": next_offset,
                    "has_more": next_offset < total_len,
                }))).into_response()
            }
            Ok(None) => {
                (StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": "Document not indexed" }))).into_response()
            }
            Err(e) => {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({ "error": format!("Failed to get content: {}", e) })),
                ).into_response()
            }
        };
    }

    match index.get_content(&query.path).await {
        Ok(Some(content)) => {
            (StatusCode::OK, Json(serde_json::json!({ "path": query.path, "content": content }))).into_response()
        }
        Ok(None) => {
            (StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": "Document not indexed" }))).into_response()
        }
        Err(e) => {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("Failed to get content: {}", e) })),
            ).into_response()
        }
    }
}

// --- Vector Search API ---

#[derive(Debug, Deserialize)]
struct VectorSearchQuery {
    query: String,
    #[serde(default = "default_top_k")]
    top_k: u32,
    file_path: Option<String>,
}

fn default_top_k() -> u32 {
    5
}

/// Semantic search over document chunks using vector embeddings.
/// Returns relevant sections ranked by similarity score.
async fn vector_search_chunks(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Query(query): Query<VectorSearchQuery>,
) -> impl IntoResponse {
    let index = match WorkspaceIndex::cached(&state.data_dir, &auth_user.user_id).await {
        Ok(idx) => idx,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("Failed to open index: {}", e) })),
            ).into_response();
        }
    };

    // Perform hybrid search (vector + FTS fallback)
    let file_paths = query.file_path.as_ref().map(|p| vec![p.clone()]);
    match index.hybrid_search(&query.query, query.top_k, file_paths.as_deref()).await {
        Ok(chunks) => {
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "query": query.query,
                    "results": chunks,
                    "count": chunks.len(),
                })),
            ).into_response()
        }
        Err(e) => {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("Search failed: {}", e) })),
            ).into_response()
        }
    }
}

// --- Document Chunks API ---

#[derive(Debug, Deserialize)]
struct DocChunksQuery {
    file_path: String,
}

/// Get all indexed chunks for a specific document file.
async fn get_document_chunks(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Query(query): Query<DocChunksQuery>,
) -> impl IntoResponse {
    let index = match WorkspaceIndex::cached(&state.data_dir, &auth_user.user_id).await {
        Ok(idx) => idx,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("Failed to open index: {}", e) })),
            ).into_response();
        }
    };

    match index.get_file_chunks(&query.file_path).await {
        Ok(chunks) => {
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "file_path": query.file_path,
                    "chunks": chunks,
                    "count": chunks.len(),
                })),
            ).into_response()
        }
        Err(e) => {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("Failed to get chunks: {}", e) })),
            ).into_response()
        }
    }
}

// --- Document Image Proxy API ---

#[derive(Debug, Deserialize)]
struct DocImageQuery {
    file_path: String,
    filename: String,
}

/// Proxy image assets from the Docling service.
///
/// Looks up the document's `doc_hash` from the workspace index, then proxies
/// the image from the Docling service's `/assets/{doc_hash}/{filename}` endpoint.
/// This allows the frontend to display PDF images without direct access to the Docling service.
async fn get_document_image(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Query(query): Query<DocImageQuery>,
) -> impl IntoResponse {
    // Validate filename to prevent path traversal
    if query.filename.contains("..") || query.filename.contains('/') || query.filename.contains('\\') {
        return (
            StatusCode::BAD_REQUEST,
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            serde_json::to_vec(&serde_json::json!({ "error": "Invalid filename" })).unwrap_or_default(),
        ).into_response();
    }

    // Look up doc_hash from workspace index
    let index = match WorkspaceIndex::cached(&state.data_dir, &auth_user.user_id).await {
        Ok(idx) => idx,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                serde_json::to_vec(&serde_json::json!({ "error": format!("Failed to open index: {}", e) })).unwrap_or_default(),
            ).into_response();
        }
    };

    let doc_hash = match index.get_doc_hash(&query.file_path).await {
        Ok(Some(hash)) => hash,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                serde_json::to_vec(&serde_json::json!({ "error": "Document not indexed or no images" })).unwrap_or_default(),
            ).into_response();
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                serde_json::to_vec(&serde_json::json!({ "error": format!("Failed to get doc hash: {}", e) })).unwrap_or_default(),
            ).into_response();
        }
    };

    // Proxy to Docling service
    let service_url = std::env::var("DOCLING_SERVICE_URL")
        .unwrap_or_else(|_| "http://localhost:50060".to_string());
    let image_url = format!("{}/assets/{}/{}", service_url, doc_hash, query.filename);

    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                serde_json::to_vec(&serde_json::json!({ "error": format!("Failed to create HTTP client: {}", e) })).unwrap_or_default(),
            ).into_response();
        }
    };

    match client.get(&image_url).send().await {
        Ok(resp) if resp.status().is_success() => {
            // Get content type from Docling response
            let content_type = resp.headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("image/png")
                .to_string();

            match resp.bytes().await {
                Ok(bytes) => (
                    StatusCode::OK,
                    [
                        (axum::http::header::CONTENT_TYPE, content_type),
                        (axum::http::header::CACHE_CONTROL, "public, max-age=86400".to_string()),
                    ],
                    bytes.to_vec(),
                ).into_response(),
                Err(e) => (
                    StatusCode::BAD_GATEWAY,
                    [(axum::http::header::CONTENT_TYPE, "application/json")],
                    serde_json::to_vec(&serde_json::json!({ "error": format!("Failed to read image: {}", e) })).unwrap_or_default(),
                ).into_response(),
            }
        }
        Ok(resp) => {
            (
                StatusCode::NOT_FOUND,
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                serde_json::to_vec(&serde_json::json!({ "error": format!("Image not found in Docling service (HTTP {})", resp.status()) })).unwrap_or_default(),
            ).into_response()
        }
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            serde_json::to_vec(&serde_json::json!({ "error": format!("Failed to fetch image from Docling: {}", e) })).unwrap_or_default(),
        ).into_response(),
    }
}

// --- Excel Database Preview API ---

/// Max rows per table returned by the excel-db preview endpoint.
const EXCEL_PREVIEW_ROWS: usize = 100;

#[derive(Debug, Deserialize)]
struct ExcelDbQuery {
    path: String,
}

/// Preview the SQLite database parsed from an uploaded Excel file.
/// Returns all tables with their schema and the first rows of each table.
/// If the database does not exist yet (e.g. file uploaded before Excel import
/// existed), it is built on demand from the source file.
async fn get_excel_db_content(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Query(query): Query<ExcelDbQuery>,
) -> impl IntoResponse {
    let ext = query.path.rsplit('.').next().unwrap_or("").to_lowercase();
    if !matches!(ext.as_str(), "xlsx" | "xls") {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "Not an Excel file" })),
        ).into_response();
    }

    let workspace_root = format!("{}/{}/workspace", state.data_dir, auth_user.user_id);
    let store = FileStore::new(&workspace_root);
    if let Err(e) = store.validate_path_public(&query.path) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": format!("Invalid path: {}", e) })),
        ).into_response();
    }

    let db_path = jcowork_storage::excel_db::db_path_for(&state.data_dir, &auth_user.user_id, &query.path);
    let src = std::path::Path::new(&workspace_root).join(&query.path);
    if !src.exists() {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "File not found in workspace" })),
        ).into_response();
    }

    // Rebuild the database when missing OR when the source Excel file is newer
    let needs_import = if !db_path.exists() {
        true
    } else {
        // Compare modification times: rebuild if source is newer than db
        let src_modified = std::fs::metadata(&src).and_then(|m| m.modified()).ok();
        let db_modified = std::fs::metadata(&db_path).and_then(|m| m.modified()).ok();
        match (src_modified, db_modified) {
            (Some(s), Some(d)) => s > d,
            _ => true, // If we can't determine times, rebuild to be safe
        }
    };

    if needs_import {
        if let Err(e) = jcowork_storage::excel_db::import_excel(
            &state.data_dir,
            &auth_user.user_id,
            &query.path,
            &workspace_root,
        ).await {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("Failed to parse Excel file: {}", e) })),
            ).into_response();
        }
    }

    match jcowork_storage::excel_db::preview_database(&db_path, EXCEL_PREVIEW_ROWS).await {
        Ok(tables) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "path": query.path,
                "db_name": jcowork_storage::excel_db::db_name_for(&query.path),
                "preview_rows": EXCEL_PREVIEW_ROWS,
                "tables": tables,
            })),
        ).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("Failed to read Excel database: {}", e) })),
        ).into_response(),
    }
}

// --- Workspace Index Re-index API ---
#[derive(Debug, Deserialize)]
struct ReindexRequest {
    /// Directory path to re-index (relative to workspace root). Defaults to "." (entire workspace).
    path: Option<String>,
}

async fn reindex_workspace_dir(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Json(body): Json<ReindexRequest>,
) -> impl IntoResponse {
    let workspace_root = format!("{}/{}/workspace", state.data_dir, auth_user.user_id);
    let store = FileStore::new(&workspace_root);
    let dir_path = body.path.as_deref().unwrap_or(".");

    // Validate path
    if let Err(e) = store.validate_path_public(dir_path) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": format!("Invalid path: {}", e) })),
        ).into_response();
    }

    // List all files recursively
    let files = match store.list_dir_recursive(dir_path).await {
        Ok(f) => f,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("Failed to list files: {}", e) })),
            ).into_response();
        }
    };

    let index = match WorkspaceIndex::cached(&state.data_dir, &auth_user.user_id).await {
        Ok(idx) => idx,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("Failed to open index: {}", e) })),
            ).into_response();
        }
    };

    let mut indexed = 0;
    let mut errors = 0;
    for file in &files {
        // Compute the full relative path
        let rel_path = if dir_path == "." {
            file.clone()
        } else {
            format!("{}/{}", dir_path, file)
        };

        match index.add_document(&rel_path, &workspace_root).await {
            Ok(()) => indexed += 1,
            Err(e) => {
                tracing::warn!(file = %rel_path, err = %e, "Failed to index file during re-index");
                errors += 1;
            }
        }
    }

    (StatusCode::OK, Json(serde_json::json!({
        "indexed": indexed,
        "errors": errors,
        "total_files": files.len(),
    }))).into_response()
}
