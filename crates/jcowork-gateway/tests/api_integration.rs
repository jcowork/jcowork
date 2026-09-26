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
    /// Shared handle to the user store (for seeding/purge assertions).
    user_store: Arc<UserStore>,
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

        let user_store_handle = user_store.clone();

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
            reset_codes: Arc::new(dashmap::DashMap::new()),
            data_dir: data_dir.clone(),
        };

        let router = router::build_router(state);

        Self {
            _temp_dir: temp_dir,
            router,
            token: None,
            user_store: user_store_handle,
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

    /// Make an authenticated request using the fixture's current token
    async fn make_request(&self, method: &str, path: &str, body: Option<serde_json::Value>) -> axum::http::Response<Body> {
        self.make_request_with_token(self.token.as_deref(), method, path, body).await
    }

    /// Make a request with an explicit bearer token (None = unauthenticated)
    async fn make_request_with_token(&self, token: Option<&str>, method: &str, path: &str, body: Option<serde_json::Value>) -> axum::http::Response<Body> {
        use tower::ServiceExt;

        let mut req_builder = Request::builder().method(method).uri(path);

        if let Some(token) = token {
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

    /// Register a new user (asserting success) and return its user_id.
    async fn register_user(&self, username: &str, password: &str, is_public: bool) -> String {
        use tower::ServiceExt;

        let register_req = Request::builder()
            .method("POST")
            .uri("/api/auth/register")
            .header("Content-Type", "application/json")
            .body(Body::from(
                json!({
                    "username": username,
                    "password": password,
                    "is_public": is_public
                })
                .to_string(),
            ))
            .unwrap();

        let res = self.router.clone().oneshot(register_req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK, "register should succeed for {}", username);
        let body_bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let resp: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        resp["user_id"].as_str().unwrap().to_string()
    }

    /// Attempt a login and return (status, body).
    async fn login_raw(&self, username: &str, password: &str) -> (StatusCode, serde_json::Value) {
        use tower::ServiceExt;

        let login_req = Request::builder()
            .method("POST")
            .uri("/api/auth/login")
            .header("Content-Type", "application/json")
            .body(Body::from(
                json!({ "username": username, "password": password }).to_string(),
            ))
            .unwrap();

        let res = self.router.clone().oneshot(login_req).await.unwrap();
        let status = res.status();
        let body_bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let resp: serde_json::Value =
            serde_json::from_slice(&body_bytes).unwrap_or(serde_json::Value::Null);
        (status, resp)
    }

    /// Seed the default admin account and return the admin's token.
    async fn seed_and_login_admin(&self) -> String {
        jcowork_gateway::auth::ensure_default_admin(&self.user_store)
            .await
            .expect("failed to seed default admin");
        let (status, resp) = self
            .login_raw(
                jcowork_gateway::auth::DEFAULT_ADMIN_USERNAME,
                jcowork_gateway::auth::DEFAULT_ADMIN_PASSWORD,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "admin login should succeed");
        assert_eq!(resp["is_admin"], json!(true), "admin login should report is_admin");
        resp["token"].as_str().unwrap().to_string()
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
async fn test_providers_save_add_modify_and_vision_flag() {
    let mut app = TestApp::new().await;
    app.register_and_login("provideradmin", "pass123").await;

    // Save two providers: one with a vision-capable model, one local without
    let save_body = json!({
        "entries": [{
            "id": "moonshot",
            "name": "Moonshot",
            "api_key": "sk-test-key",
            "base_url": "https://api.moonshot.cn/v1",
            "default_model": "kimi-k2.7-code",
            "context_length": 256000,
            "models": [
                {"id": "kimi-k2.6", "name": "Kimi K2.6", "context_length": 256000, "vision": true},
                {"id": "kimi-k2.7-code", "name": "Kimi K2.7", "context_length": 256000}
            ]
        }, {
            "id": "llamacpp",
            "name": "Local",
            "api_key": "",
            "base_url": "http://localhost:20261/v1",
            "default_model": "qwen3.5-35b-a3b",
            "context_length": 131072,
            "models": [
                {"id": "qwen3.5-35b-a3b", "name": "Qwen3.5 35B-A3B", "context_length": 131072, "vision": false}
            ]
        }]
    });
    let res = app.make_request("POST", "/api/providers", Some(save_body)).await;
    assert_eq!(res.status(), StatusCode::OK);

    // GET /api/providers/entries: vision flags must be persisted, key masked
    let res = app.make_request("GET", "/api/providers/entries", None).await;
    assert_eq!(res.status(), StatusCode::OK);
    let body_bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let resp: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
    let entries = resp["entries"].as_array().expect("entries array");
    assert_eq!(entries.len(), 2);

    let moonshot = entries.iter().find(|e| e["id"] == "moonshot").unwrap();
    assert_eq!(moonshot["api_key"], "*******-key", "api key must be masked");
    let models = moonshot["models"].as_array().unwrap();
    let k26 = models.iter().find(|m| m["id"] == "kimi-k2.6").unwrap();
    assert_eq!(k26["vision"], true, "vision flag must persist for kimi-k2.6");
    let k27 = models.iter().find(|m| m["id"] == "kimi-k2.7-code").unwrap();
    assert_eq!(k27["vision"], false, "missing vision flag must default to false");

    // GET /api/providers: the rebuilt router must expose vision in model info
    let res = app.make_request("GET", "/api/providers", None).await;
    assert_eq!(res.status(), StatusCode::OK);
    let body_bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let resp: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
    let providers = resp["providers"].as_array().expect("providers array");
    let moonshot = providers.iter().find(|p| p["id"] == "moonshot").unwrap();
    let models = moonshot["models"].as_array().unwrap();
    let k26 = models.iter().find(|m| m["id"] == "kimi-k2.6").unwrap();
    assert_eq!(k26["vision"], true, "rebuilt router must keep vision flag");

    // Modify: toggle kimi-k2.7-code to vision, add a new provider, drop llamacpp
    let save_body = json!({
        "entries": [{
            "id": "moonshot",
            "name": "Moonshot",
            "api_key": "sk-test-key",
            "base_url": "https://api.moonshot.cn/v1",
            "default_model": "kimi-k2.7-code",
            "context_length": 256000,
            "models": [
                {"id": "kimi-k2.6", "name": "Kimi K2.6", "context_length": 256000, "vision": true},
                {"id": "kimi-k2.7-code", "name": "Kimi K2.7", "context_length": 256000, "vision": true}
            ]
        }, {
            "id": "newprovider",
            "name": "New Provider",
            "api_key": "sk-new-key",
            "base_url": "https://api.example.com/v1",
            "default_model": "vision-model",
            "context_length": 128000,
            "models": [
                {"id": "vision-model", "name": "Vision Model", "context_length": 128000, "vision": true}
            ]
        }]
    });
    let res = app.make_request("POST", "/api/providers", Some(save_body)).await;
    assert_eq!(res.status(), StatusCode::OK);

    // Verify modification took effect everywhere
    let res = app.make_request("GET", "/api/providers", None).await;
    let body_bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let resp: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
    let providers = resp["providers"].as_array().unwrap();
    assert!(providers.iter().any(|p| p["id"] == "newprovider"), "new provider must appear");
    assert!(!providers.iter().any(|p| p["id"] == "llamacpp"), "removed provider must be gone");
    let moonshot = providers.iter().find(|p| p["id"] == "moonshot").unwrap();
    let k27 = moonshot["models"].as_array().unwrap().iter()
        .find(|m| m["id"] == "kimi-k2.7-code").unwrap();
    assert_eq!(k27["vision"], true, "toggled vision flag must take effect");
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

#[tokio::test]
async fn test_skill_config_save_get_clear() {
    let mut app = TestApp::new().await;
    app.register_and_login("skillcfg", "pass123").await;

    let skill_id = "builtin:image_to_html";
    let cfg_url = format!("/api/skills/{}/config", skill_id);

    // Initially unset
    let res = app.make_request("GET", &cfg_url, None).await;
    assert_eq!(res.status(), StatusCode::OK);
    let body: serde_json::Value =
        serde_json::from_slice(&axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap())
            .unwrap();
    assert_eq!(body["vl_model"], serde_json::Value::Null);

    // Save a VL model selection
    let res = app
        .make_request("PUT", &cfg_url, Some(json!({"vl_model": "moonshot:kimi-k2.6"})))
        .await;
    assert_eq!(res.status(), StatusCode::OK);

    let res = app.make_request("GET", &cfg_url, None).await;
    let body: serde_json::Value =
        serde_json::from_slice(&axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap())
            .unwrap();
    assert_eq!(body["vl_model"], "moonshot:kimi-k2.6");

    // Overwrite with another model: exactly one entry must remain
    let res = app
        .make_request("PUT", &cfg_url, Some(json!({"vl_model": "moonshot:kimi-k3"})))
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    let res = app.make_request("GET", &cfg_url, None).await;
    let body: serde_json::Value =
        serde_json::from_slice(&axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap())
            .unwrap();
    assert_eq!(body["vl_model"], "moonshot:kimi-k3");

    // Clear the selection (null), and empty string also clears
    let res = app.make_request("PUT", &cfg_url, Some(json!({"vl_model": null}))).await;
    assert_eq!(res.status(), StatusCode::OK);
    let res = app.make_request("GET", &cfg_url, None).await;
    let body: serde_json::Value =
        serde_json::from_slice(&axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap())
            .unwrap();
    assert_eq!(body["vl_model"], serde_json::Value::Null);
}

// ─────────────────────────────────────────────────────────────────────────────
// Admin user management & recycle bin
// ─────────────────────────────────────────────────────────────────────────────

/// Full lifecycle: trash → login blocked → restore → login works → purge record.
#[tokio::test]
async fn test_admin_user_management_flow() {
    let app = TestApp::new().await;
    let admin_token = app.seed_and_login_admin().await;

    // A regular user cannot access the admin API
    let victim_id = app.register_user("victim", "victimpass1", false).await;
    let (status, victim_login) = app.login_raw("victim", "victimpass1").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(victim_login["is_admin"], json!(false));
    let victim_token = victim_login["token"].as_str().unwrap().to_string();

    let res = app
        .make_request_with_token(Some(&victim_token), "GET", "/api/admin/users", None)
        .await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN, "non-admin must be rejected");

    // Admin sees the victim in the active list, without any password hash
    let res = app
        .make_request_with_token(Some(&admin_token), "GET", "/api/admin/users?status=active", None)
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    let body: serde_json::Value =
        serde_json::from_slice(&axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap())
            .unwrap();
    let items = body.as_array().unwrap();
    assert!(items.iter().any(|u| u["user_id"] == json!(victim_id)));
    assert!(items.iter().all(|u| u.get("password_hash").is_none()));

    // Move the victim to the trash
    let trash_url = format!("/api/admin/users/{}/trash", victim_id);
    let res = app
        .make_request_with_token(Some(&admin_token), "POST", &trash_url, None)
        .await;
    assert_eq!(res.status(), StatusCode::OK);

    // Trashed user cannot log in, and the old token is rejected
    let (status, resp) = app.login_raw("victim", "victimpass1").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "trashed user login must fail");
    assert_eq!(resp["error"], json!("Account has been deleted"));
    let res = app
        .make_request_with_token(Some(&victim_token), "GET", "/api/providers", None)
        .await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN, "trashed user token must be rejected");

    // The trash list shows the victim with days_left and deleted_at
    let res = app
        .make_request_with_token(Some(&admin_token), "GET", "/api/admin/users?status=trash", None)
        .await;
    let body: serde_json::Value =
        serde_json::from_slice(&axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap())
            .unwrap();
    let trashed = body
        .as_array()
        .unwrap()
        .iter()
        .find(|u| u["user_id"] == json!(victim_id))
        .expect("victim should be in the trash list");
    assert_eq!(trashed["days_left"], json!(7));
    assert!(!trashed["deleted_at"].is_null());

    // Trashing again conflicts
    let res = app
        .make_request_with_token(Some(&admin_token), "POST", &trash_url, None)
        .await;
    assert_eq!(res.status(), StatusCode::CONFLICT);

    // The admin account itself can never be trashed
    let res = app
        .make_request_with_token(Some(&admin_token), "GET", "/api/admin/users?status=active", None)
        .await;
    let body: serde_json::Value =
        serde_json::from_slice(&axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap())
            .unwrap();
    let admin_id = body
        .as_array()
        .unwrap()
        .iter()
        .find(|u| u["username"] == json!("admin"))
        .unwrap()["user_id"]
        .as_str()
        .unwrap()
        .to_string();
    let res = app
        .make_request_with_token(
            Some(&admin_token),
            "POST",
            &format!("/api/admin/users/{}/trash", admin_id),
            None,
        )
        .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    // Restore: the victim can log in again with the same token flow
    let restore_url = format!("/api/admin/users/{}/restore", victim_id);
    let res = app
        .make_request_with_token(Some(&admin_token), "POST", &restore_url, None)
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    let (status, _) = app.login_raw("victim", "victimpass1").await;
    assert_eq!(status, StatusCode::OK, "restored user must be able to log in");
    let res = app
        .make_request_with_token(Some(&victim_token), "GET", "/api/providers", None)
        .await;
    assert_eq!(res.status(), StatusCode::OK, "old token must work again after restore");

    // Permanently delete the account; login falls back to invalid credentials
    let res = app
        .make_request_with_token(Some(&admin_token), "DELETE", &format!("/api/admin/users/{}", victim_id), None)
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    let (status, _) = app.login_raw("victim", "victimpass1").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "deleted account must not exist");
    let res = app
        .make_request_with_token(Some(&admin_token), "DELETE", &format!("/api/admin/users/{}", admin_id), None)
        .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST, "admin account cannot be deleted");
}

/// Trashed public accounts disappear from the public directory and 404 on profile access.
#[tokio::test]
async fn test_public_users_excludes_trashed() {
    let app = TestApp::new().await;
    let admin_token = app.seed_and_login_admin().await;

    let pub_id = app.register_user("pubvictim", "pubpass1234", true).await;
    app.register_user("regular", "regularpass1", false).await;
    let (status, regular_login) = app.login_raw("regular", "regularpass1").await;
    assert_eq!(status, StatusCode::OK);
    let regular_token = regular_login["token"].as_str().unwrap().to_string();

    // Public directory contains the user while active
    let res = app
        .make_request_with_token(Some(&regular_token), "GET", "/api/public-users", None)
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    let body: serde_json::Value =
        serde_json::from_slice(&axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap())
            .unwrap();
    assert!(body
        .as_array()
        .unwrap()
        .iter()
        .any(|u| u["user_id"] == json!(pub_id)));

    // Trash the public user
    let res = app
        .make_request_with_token(
            Some(&admin_token),
            "POST",
            &format!("/api/admin/users/{}/trash", pub_id),
            None,
        )
        .await;
    assert_eq!(res.status(), StatusCode::OK);

    // Directory no longer lists it, and profile access returns 404
    let res = app
        .make_request_with_token(Some(&regular_token), "GET", "/api/public-users", None)
        .await;
    let body: serde_json::Value =
        serde_json::from_slice(&axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap())
            .unwrap();
    assert!(
        !body
            .as_array()
            .unwrap()
            .iter()
            .any(|u| u["user_id"] == json!(pub_id)),
        "trashed public user must be invisible in the directory"
    );
    let res = app
        .make_request_with_token(
            Some(&regular_token),
            "GET",
            &format!("/api/public-users/{}/documents", pub_id),
            None,
        )
        .await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND, "trashed public profile must 404");

    // Restore brings it back
    let res = app
        .make_request_with_token(
            Some(&admin_token),
            "POST",
            &format!("/api/admin/users/{}/restore", pub_id),
            None,
        )
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    let res = app
        .make_request_with_token(Some(&regular_token), "GET", "/api/public-users", None)
        .await;
    let body: serde_json::Value =
        serde_json::from_slice(&axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap())
            .unwrap();
    assert!(body
        .as_array()
        .unwrap()
        .iter()
        .any(|u| u["user_id"] == json!(pub_id)));
}

/// `purge_expired_trash` keeps fresh trashed users and deletes expired ones.
#[tokio::test]
async fn test_trash_purge_expired_trash() {
    let app = TestApp::new().await;

    // Create a user directly in the store and trash it
    let hash = jcowork_gateway::auth::hash_password("purgepass123").unwrap();
    let user = app.user_store.create_user("purgevictim", &hash, false).await.unwrap();
    app.user_store.soft_delete_user(&user.id).await.unwrap();

    // Fresh trash entry is NOT purged with the 7-day retention window
    let purged = app.user_store.purge_expired_trash(7).await.unwrap();
    assert_eq!(purged, 0, "fresh trash entry must survive the 7-day purge");
    assert!(app.user_store.get_user_by_id(&user.id).await.unwrap().is_some());

    // With a zero-day window the same entry counts as expired and is removed
    let purged = app.user_store.purge_expired_trash(0).await.unwrap();
    assert!(purged >= 1, "expired trash entry must be purged");
    assert!(app.user_store.get_user_by_id(&user.id).await.unwrap().is_none());

    // Active (non-trashed) users are never purged
    let active = app.user_store.create_user("activeuser", &hash, false).await.unwrap();
    let purged = app.user_store.purge_expired_trash(0).await.unwrap();
    assert_eq!(purged, 0, "active users must never be purged");
    assert!(app.user_store.get_user_by_id(&active.id).await.unwrap().is_some());
}

/// `ensure_default_admin` is idempotent and promotes an existing `admin` account.
#[tokio::test]
async fn test_ensure_default_admin_idempotent() {
    let app = TestApp::new().await;

    // Pre-create an account named "admin"; seeding must promote it, not duplicate
    let hash = jcowork_gateway::auth::hash_password("someOtherPass").unwrap();
    let existing = app.user_store.create_user("admin", &hash, false).await.unwrap();
    assert!(!existing.is_admin);

    jcowork_gateway::auth::ensure_default_admin(&app.user_store).await.unwrap();
    let promoted = app.user_store.get_user_by_username("admin").await.unwrap().unwrap();
    assert!(promoted.is_admin, "existing admin account must be promoted");
    assert_eq!(promoted.id, existing.id, "no duplicate admin account may be created");

    // Second call is a no-op
    jcowork_gateway::auth::ensure_default_admin(&app.user_store).await.unwrap();
    let again = app.user_store.get_user_by_username("admin").await.unwrap().unwrap();
    assert!(again.is_admin);
    assert_eq!(again.id, existing.id);
}
