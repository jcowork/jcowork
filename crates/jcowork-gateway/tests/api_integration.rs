//! API Integration Tests for jcowork-gateway
//!
//! These tests verify the core HTTP API endpoints:
//! - Authentication (register, login)
//! - Provider management (list)
//! - Health endpoint

use axum::{
    body::Body,
    http::{Request, StatusCode},
    Router,
};
use serde_json::json;
use std::sync::{Arc, RwLock};

use jcowork_gateway::{
    auth::AuthConfig,
    router::{self, AppState},
    session::SessionManager,
};
use jcowork_llm::{LlmRouter, MockLlmProvider};
use jcowork_logs::LogWriter;
use jcowork_memory::{BuiltinMemoryProvider, MemoryManager};
use jcowork_skills::SkillManager;
use jcowork_storage::{FeishuConfigStore, UserStore};
use jcowork_tools::registry::ToolRegistry;
use sqlx::sqlite::SqlitePoolOptions;
use tempfile::TempDir;

/// Test fixture that sets up a minimal gateway with in-memory SQLite
struct TestApp {
    _temp_dir: TempDir,
    router: Router,
    token: Option<String>,
}

impl TestApp {
    async fn new() -> Self {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let data_dir = temp_dir.path().to_str().unwrap().to_string();

        // Create SQLite pool
        let db_path = format!("{}/test.db", data_dir);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect(&format!("sqlite:{}?mode=rwc", db_path))
            .await
            .expect("Failed to create SQLite pool");

        // Run migrations
        jcowork_storage::migration::run_migrations(&pool)
            .await
            .expect("Failed to run migrations");

        // Initialize components
        let user_store = Arc::new(UserStore::new(&data_dir).await.unwrap());
        let log_writer = Arc::new(LogWriter::new_disabled());
        let memory_provider = BuiltinMemoryProvider::new(pool.clone());
        memory_provider.init().await.unwrap();
        let mut memory_manager = MemoryManager::new();
        memory_manager.add_provider(Arc::new(memory_provider));
        let memory_manager = Arc::new(memory_manager);
        let skill_manager = Arc::new(SkillManager::new(pool.clone()));
        let tool_registry = Arc::new(ToolRegistry::new());
        let session_manager = Arc::new(SessionManager::new());
        let feishu_config_store = Arc::new(FeishuConfigStore::new(pool.clone()));

        // Connector manager (user-managed API/MCP tools)
        let connector_manager = jcowork_connectors::ConnectorManager::new(pool.clone());
        connector_manager.attach_registry(tool_registry.clone()).await;

        // Create mock LLM router
        let mock_provider = Arc::new(MockLlmProvider::new());
        let llm_router = LlmRouter::from_mock(mock_provider);

        let state = AppState {
            session_manager,
            auth_config: AuthConfig {
                jwt_secret: "test-secret".to_string(),
                token_duration_hours: 24,
            },
            llm_router: Arc::new(RwLock::new(llm_router)),
            default_model: "mock:test-model".to_string(),
            cron_scheduler: Arc::new(jcowork_cron::CronScheduler::new()),
            memory_manager,
            skill_manager,
            tool_registry,
            connector_manager,
            user_store,
            log_writer,
            feishu_config_store,
            feishu_client_cache: Arc::new(dashmap::DashMap::new()),
            data_dir: data_dir.clone(),
        };

        let router = router::build_router(state);

        Self {
            _temp_dir: temp_dir,
            router,
            token: None,
        }
    }

    /// Register a test user and get JWT token
    async fn register_and_login(&mut self, username: &str, password: &str) {
        use tower::ServiceExt;
        
        // Register
        let register_req = Request::builder()
            .method("POST")
            .uri("/api/auth/register")
            .header("Content-Type", "application/json")
            .body(Body::from(
                json!({
                    "username": username,
                    "password": password
                })
                .to_string(),
            ))
            .unwrap();

        let register_res = self.router.clone().oneshot(register_req).await.unwrap();
        assert_eq!(register_res.status(), StatusCode::OK);

        // Login
        let login_req = Request::builder()
            .method("POST")
            .uri("/api/auth/login")
            .header("Content-Type", "application/json")
            .body(Body::from(
                json!({
                    "username": username,
                    "password": password
                })
                .to_string(),
            ))
            .unwrap();

        let login_res = self.router.clone().oneshot(login_req).await.unwrap();
        assert_eq!(login_res.status(), StatusCode::OK);

        let body_bytes = axum::body::to_bytes(login_res.into_body(), usize::MAX)
            .await
            .unwrap();
        let login_resp: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        self.token = Some(login_resp["token"].as_str().unwrap().to_string());
    }

    /// Make an authenticated request
    async fn make_request(&self, method: &str, path: &str, body: Option<serde_json::Value>) -> axum::http::Response<Body> {
        use tower::ServiceExt;
        
        let mut req_builder = Request::builder().method(method).uri(path);

        if let Some(token) = &self.token {
            req_builder = req_builder.header("Authorization", format!("Bearer {}", token));
        }

        let body_str = match body {
            Some(b) => b.to_string(),
            None => String::new(),
        };

        let req = req_builder
            .header("Content-Type", "application/json")
            .body(Body::from(body_str))
            .unwrap();

        self.router.clone().oneshot(req).await.unwrap()
    }
}

#[tokio::test]
async fn test_auth_register_login_flow() {
    let mut app = TestApp::new().await;

    // Test registration
    app.register_and_login("testuser", "securepass123").await;

    // Verify token is set
    assert!(app.token.is_some());
    assert!(!app.token.as_ref().unwrap().is_empty());
}

#[tokio::test]
async fn test_auth_invalid_credentials() {
    use tower::ServiceExt;
    
    let mut app = TestApp::new().await;

    // Register first
    app.register_and_login("testuser2", "pass123").await;

    // Try login with wrong password
    let login_req = Request::builder()
        .method("POST")
        .uri("/api/auth/login")
        .header("Content-Type", "application/json")
        .body(Body::from(
            json!({
                "username": "testuser2",
                "password": "wrongpassword"
            })
            .to_string(),
        ))
        .unwrap();

    let login_res = app.router.clone().oneshot(login_req).await.unwrap();
    assert_eq!(login_res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_providers_list_without_auth() {
    use tower::ServiceExt;
    
    let app = TestApp::new().await;

    // Try to list providers without authentication
    let req = Request::builder()
        .method("GET")
        .uri("/api/providers")
        .body(Body::empty())
        .unwrap();

    let res = app.router.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_providers_list_with_auth() {
    let mut app = TestApp::new().await;
    app.register_and_login("provideruser", "pass123").await;

    // List providers with valid token
    let res = app.make_request("GET", "/api/providers", None).await;

    assert_eq!(res.status(), StatusCode::OK);

    let body_bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let resp: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();

    // Verify response structure
    assert!(resp.get("providers").is_some());
    assert!(resp.get("default_model").is_some());
}

#[tokio::test]
async fn test_health_endpoint_public() {
    use tower::ServiceExt;
    
    let app = TestApp::new().await;

    // Health endpoint should be public (no auth required)
    let req = Request::builder()
        .method("GET")
        .uri("/api/health")
        .body(Body::empty())
        .unwrap();

    let res = app.router.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_401_triggers_frontend_logout() {
    use tower::ServiceExt;
    
    // This test documents the expected behavior:
    // When API returns 401, frontend's global fetch interceptor should:
    // 1. Clear localStorage.removeItem('jcowork_auth')
    // 2. Reload page to redirect to login
    //
    // The actual clearing logic is in web/src/App.tsx global fetch wrapper
    
    let app = TestApp::new().await;

    // Make unauthenticated request to protected endpoint
    let req = Request::builder()
        .method("GET")
        .uri("/api/providers")
        .body(Body::empty())
        .unwrap();

    let res = app.router.oneshot(req).await.unwrap();
    
    // Should return 401, which triggers frontend auto-logout
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    
    let body_bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let resp: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
    
    // Verify error message
    assert!(resp.get("error").is_some());
}

// ─── Connector API tests ────────────────────────────────────────────

fn api_connector_body() -> serde_json::Value {
    json!({
        "name": "weather",
        "ctype": "api",
        "description": "Weather service",
        "config": {
            "tools": [{
                "name": "get_weather",
                "description": "Get current weather for a city",
                "method": "GET",
                "url": "https://api.example.com/weather?city={{city}}",
                "params": {
                    "type": "object",
                    "properties": {"city": {"type": "string"}},
                    "required": ["city"]
                }
            }]
        }
    })
}

#[tokio::test]
async fn test_connectors_require_auth() {
    use tower::ServiceExt;

    let app = TestApp::new().await;
    let req = Request::builder()
        .method("GET")
        .uri("/api/connectors")
        .body(Body::empty())
        .unwrap();
    let res = app.router.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_connector_crud_flow() {
    let mut app = TestApp::new().await;
    app.register_and_login("connectoruser", "pass123").await;

    // Create
    let res = app
        .make_request("POST", "/api/connectors", Some(api_connector_body()))
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    let body: serde_json::Value =
        serde_json::from_slice(&axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap())
            .unwrap();
    let id = body["id"].as_str().unwrap().to_string();
    assert!(body["enabled"].as_bool().unwrap());

    // List
    let res = app.make_request("GET", "/api/connectors", None).await;
    assert_eq!(res.status(), StatusCode::OK);
    let body: serde_json::Value =
        serde_json::from_slice(&axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap())
            .unwrap();
    assert_eq!(body.as_array().unwrap().len(), 1);

    // Tools list
    let res = app
        .make_request("GET", &format!("/api/connectors/{}/tools", id), None)
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    let body: serde_json::Value =
        serde_json::from_slice(&axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap())
            .unwrap();
    assert_eq!(body[0]["name"], "get_weather");
    assert_eq!(body[0]["enabled"], true);

    // Tool-level toggle (disable)
    let res = app
        .make_request(
            "POST",
            &format!("/api/connectors/{}/tools/get_weather/toggle", id),
            Some(json!({"enabled": false})),
        )
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    let res = app
        .make_request("GET", &format!("/api/connectors/{}/tools", id), None)
        .await;
    let body: serde_json::Value =
        serde_json::from_slice(&axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap())
            .unwrap();
    assert_eq!(body[0]["enabled"], false);

    // Connector-level toggle (disable)
    let res = app
        .make_request(
            "POST",
            &format!("/api/connectors/{}/toggle", id),
            Some(json!({"enabled": false})),
        )
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    let res = app
        .make_request("GET", &format!("/api/connectors/{}", id), None)
        .await;
    let body: serde_json::Value =
        serde_json::from_slice(&axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap())
            .unwrap();
    assert_eq!(body["enabled"], false);

    // Update preserves enabled state
    let mut update_body = api_connector_body();
    update_body["name"] = json!("weather-v2");
    let res = app
        .make_request("PUT", &format!("/api/connectors/{}", id), Some(update_body))
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    let body: serde_json::Value =
        serde_json::from_slice(&axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap())
            .unwrap();
    assert_eq!(body["name"], "weather-v2");
    assert_eq!(body["enabled"], false, "update must preserve enabled state");

    // Delete
    let res = app
        .make_request("DELETE", &format!("/api/connectors/{}", id), None)
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    let res = app.make_request("GET", "/api/connectors", None).await;
    let body: serde_json::Value =
        serde_json::from_slice(&axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap())
            .unwrap();
    assert_eq!(body.as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn test_connector_validation_errors() {
    let mut app = TestApp::new().await;
    app.register_and_login("connvalidator", "pass123").await;

    // Empty name -> 400
    let mut body = api_connector_body();
    body["name"] = json!("  ");
    let res = app.make_request("POST", "/api/connectors", Some(body)).await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    // Undeclared placeholder -> 400
    let mut body = api_connector_body();
    body["config"]["tools"][0]["url"] = json!("https://x.com/{{undeclared}}");
    let res = app.make_request("POST", "/api/connectors", Some(body)).await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    // Unknown connector type -> 400
    let mut body = api_connector_body();
    body["ctype"] = json!("carrier-pigeon");
    let res = app.make_request("POST", "/api/connectors", Some(body)).await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    // MCP with invalid transport config -> 400
    let res = app
        .make_request(
            "POST",
            "/api/connectors",
            Some(json!({
                "name": "bad-mcp",
                "ctype": "mcp",
                "config": {"transport": "http", "url": "not-a-url"}
            })),
        )
        .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    // API connector test endpoint validates without saving
    let res = app
        .make_request("POST", "/api/connectors/test", Some(api_connector_body()))
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    let body: serde_json::Value =
        serde_json::from_slice(&axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap())
            .unwrap();
    assert_eq!(body["status"], "ok");

    // Nothing was persisted
    let res = app.make_request("GET", "/api/connectors", None).await;
    let body: serde_json::Value =
        serde_json::from_slice(&axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap())
            .unwrap();
    assert_eq!(body.as_array().unwrap().len(), 0);
}
