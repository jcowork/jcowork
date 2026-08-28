//! REST API routes.
//!
//! Handlers are split into domain-specific submodules:
//! - [`auth_api`] — registration, login, health
//! - [`skills`] — skill listing / toggling
//! - [`memory`] — memory CRUD + agent identity
//! - [`cron`] — reminders and periodic tasks
//! - [`providers`] — LLM provider management
//! - [`feishu`] — per-user Feishu configuration
//! - [`workspace`] — workspace file management
//! - [`upload`] — file/PDF uploads and PDF parsing
//! - [`fetch_url`] — URL fetching and HTML-to-text conversion
//! - [`doc_index`] — workspace index, vector search, excel preview, docling

pub(crate) mod auth_api;
pub(crate) mod cron;
pub(crate) mod doc_index;
pub(crate) mod feishu;
pub(crate) mod fetch_url;
pub(crate) mod memory;
pub(crate) mod providers;
pub(crate) mod skills;
pub(crate) mod upload;
pub(crate) mod workspace;

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
use axum::extract::ws::WebSocketUpgrade;
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
use jcowork_llm::LlmRouter;
use jcowork_logs::LogWriter;
use jcowork_memory::MemoryManager;
use jcowork_skills::SkillManager;
use jcowork_storage::{FeishuConfigStore, UserStore};
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
    pub query: Option<String>,
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateMemoryRequest {
    pub content: Option<String>,
    pub category: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RegisterRequest {
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
    pub token: String,
}

/// Mask a secret string, showing only the last 4 characters.
pub(crate) fn mask_secret(secret: &str) -> String {
    if secret.len() <= 4 {
        "****".to_string()
    } else {
        format!("{}{}", "*".repeat(secret.len() - 4), &secret[secret.len() - 4..])
    }
}

// --- Route assembly ---

pub fn build_router(state: AppState) -> Router {
    let auth_mw = axum::middleware::from_fn_with_state(state.clone(), auth_middleware);

    // Public routes (no auth required)
    let public = Router::new()
        .route("/api/auth/register", post(auth_api::register))
        .route("/api/auth/login", post(auth_api::login))
        .route("/api/health", get(auth_api::health))
        .route("/api/feishu/event", post(crate::feishu::feishu_event_handler));

    // Protected routes (auth required)
    let protected = Router::new()
        .route("/api/chat", post(chat))
        .route("/api/sessions", get(list_sessions))
        .route("/api/skills", get(skills::list_skills))
        .route("/api/skills", post(skills::create_skill))
        .route("/api/skills/all", get(skills::list_all_skills))
        .route("/api/skills/{id}/toggle", put(skills::toggle_skill))
        .route("/api/memory", get(memory::list_memories))
        .route("/api/memory/search", get(memory::search_memories))
        .route("/api/memory/{id}", put(memory::update_memory))
        .route("/api/memory/{id}", delete(memory::delete_memory))
        .route("/api/reminders", get(cron::list_reminders))
        .route("/api/reminders/{id}", delete(cron::remove_reminder))
        .route("/api/cron-jobs", get(cron::list_cron_jobs))
        .route("/api/cron-jobs", post(cron::create_cron_job))
        .route("/api/cron-jobs/{id}", delete(cron::remove_cron_job))
        .route("/api/cron-jobs/{id}/results", get(cron::get_cron_job_results))
        .route("/api/cron-jobs/{id}/results", post(cron::store_cron_job_result))
        .route("/api/providers", get(providers::list_providers))
        .route("/api/providers/entries", get(providers::list_provider_entries))
        .route("/api/providers", post(providers::save_providers))
        .route("/api/agent-identity", get(memory::get_agent_identity))
        .route("/api/agent-identity", put(memory::set_agent_identity))
        .route("/api/feishu/config", get(feishu::get_feishu_config))
        .route("/api/feishu/config", put(feishu::set_feishu_config))
        .route("/api/feishu/config", delete(feishu::delete_feishu_config))
        .route("/api/workspace/files", get(workspace::list_workspace_files))
        .route("/api/workspace/download", get(workspace::download_workspace_file))
        .route("/api/workspace/upload-pdf", post(upload::upload_pdf))
        .route("/api/workspace/parse-pdf", post(upload::parse_workspace_pdf))
        .route("/api/workspace/upload", post(upload::upload_file))
        .route("/api/workspace/mkdir", post(workspace::create_directory))
        .route("/api/workspace/files-recursive", get(workspace::list_workspace_files_recursive))
        .route("/api/workspace/delete", post(workspace::delete_workspace_file))
        .route("/api/workspace/move", post(workspace::move_workspace_path))
        .route("/api/workspace/save", post(workspace::save_workspace_file))
        .route("/api/workspace/index/search", get(doc_index::search_workspace_index))
        .route("/api/workspace/index/list", get(doc_index::list_workspace_index))
        .route("/api/workspace/index/content", get(doc_index::get_indexed_content))
        .route("/api/workspace/index/reindex", post(doc_index::reindex_workspace_dir))
        .route("/api/workspace/vector/search", get(doc_index::vector_search_chunks))
        .route("/api/workspace/doc/chunks", get(doc_index::get_document_chunks))
        .route("/api/workspace/doc/image", get(doc_index::get_document_image))
        .route("/api/workspace/excel-db", get(doc_index::get_excel_db_content))
        .route("/api/docling/status", get(doc_index::get_docling_status))
        .route("/api/docling/start", post(doc_index::start_docling_service))
        .route("/api/fetch-url", post(fetch_url::fetch_url))
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
                token,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mask_secret_short() {
        assert_eq!(mask_secret(""), "****");
        assert_eq!(mask_secret("abcd"), "****");
        assert_eq!(mask_secret("ab"), "****");
    }

    #[test]
    fn test_mask_secret_long() {
        assert_eq!(mask_secret("abcdefgh"), "****efgh");
        assert_eq!(mask_secret("sk-1234567890"), "*********7890");
    }

    #[test]
    fn test_mask_secret_boundary_five_chars() {
        assert_eq!(mask_secret("abcde"), "*bcde");
    }
}
