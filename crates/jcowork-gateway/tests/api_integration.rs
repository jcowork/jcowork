//! API Integration Tests for jcowork-gateway
//!
//! These tests verify the core HTTP API endpoints:
//! - Authentication (SMS verification-code login, email verification login,
//!   admin bootstrap)
//! - Provider management (list)
//! - Health endpoint

use axum::{
    body::Body,
    http::{Request, StatusCode},
    Router,
};
use serde_json::json;
use std::sync::atomic::{AtomicU32, Ordering};
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

/// Records every verification code the gateway hands to the carrier, so tests
/// can complete an SMS login without a real channel.
///
/// This lives in the test binary rather than in `jcowork-sms`: a fixed or
/// readable code path in production code would be a universal backdoor.
#[derive(Default)]
struct CaptureSms {
    sent: Arc<std::sync::Mutex<Vec<(String, String)>>>,
}

#[async_trait::async_trait]
impl jcowork_sms::SmsSender for CaptureSms {
    async fn send_code(&self, phone: &str, code: &str) -> anyhow::Result<()> {
        self.sent
            .lock()
            .expect("capture lock poisoned")
            .push((phone.to_string(), code.to_string()));
        Ok(())
    }

    fn name(&self) -> &'static str {
        "capture"
    }
}

/// Records every verification code the gateway hands to the mail channel, so
/// tests can complete an email flow without SMTP. Same rule as `CaptureSms`:
/// capture lives in the test binary, never in production code.
#[derive(Default)]
struct CaptureMail {
    sent: Arc<std::sync::Mutex<Vec<(String, String)>>>,
}

#[async_trait::async_trait]
impl jcowork_mail::MailSender for CaptureMail {
    async fn send_code(&self, email: &str, code: &str) -> anyhow::Result<()> {
        self.sent
            .lock()
            .expect("capture lock poisoned")
            .push((email.to_string(), code.to_string()));
        Ok(())
    }

    fn name(&self) -> &'static str {
        "capture"
    }
}

/// Test fixture that sets up a minimal gateway with in-memory SQLite
struct TestApp {
    _temp_dir: TempDir,
    router: Router,
    token: Option<String>,
    /// Shared handle to the user store (for seeding/purge assertions).
    user_store: Arc<UserStore>,
    /// Codes captured from the fake SMS channel, as (phone, code) pairs.
    sms_sent: Arc<std::sync::Mutex<Vec<(String, String)>>>,
    /// Codes captured from the fake mail channel, as (email, code) pairs.
    mail_sent: Arc<std::sync::Mutex<Vec<(String, String)>>>,
    /// Counter behind `next_phone`.
    phone_seq: AtomicU32,
    /// Counter behind `next_email`.
    email_seq: AtomicU32,
}

impl TestApp {
    async fn new() -> Self {
        Self::with_sms(jcowork_sms::config::SmsConfig::default()).await
    }

    /// Build the test app with a specific SMS configuration, so tests can
    /// exercise the daily cap and the invite-only (`allow_register = false`)
    /// mode without touching the process environment.
    async fn with_sms(config: jcowork_sms::config::SmsConfig) -> Self {
        Self::build(config, true).await
    }

    /// Build the test app with email registration turned off
    /// (`JCWORK_MAIL_ALLOW_REGISTER=false`).
    async fn with_email_registration(allow: bool) -> Self {
        Self::build(jcowork_sms::config::SmsConfig::default(), allow).await
    }

    async fn build(
        config: jcowork_sms::config::SmsConfig,
        mail_allow_register: bool,
    ) -> Self {
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

        let capture = CaptureSms::default();
        let sms_sent = capture.sent.clone();
        let sms = router::SmsRuntime::new(
            Arc::new(capture),
            config.daily_global_cap,
            config.allow_register,
        );

        let mail_capture = CaptureMail::default();
        let mail_sent = mail_capture.sent.clone();
        let mail = router::MailRuntime::new(Arc::new(mail_capture), 500, mail_allow_register);

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
            sms,
            mail,
            data_dir: data_dir.clone(),
        };

        let router = router::build_router(state);

        Self {
            _temp_dir: temp_dir,
            router,
            token: None,
            user_store: user_store_handle,
            sms_sent,
            mail_sent,
            phone_seq: AtomicU32::new(0),
            email_seq: AtomicU32::new(0),
        }
    }

    /// Next unused valid phone number.
    ///
    /// Every account a test needs must be provisioned on a fresh number: the
    /// 60-second resend cooldown means a given number yields exactly one code.
    fn next_phone(&self) -> String {
        format!("137{:08}", self.phone_seq.fetch_add(1, Ordering::SeqCst))
    }

    /// Next unused email address (fresh per test for the same cooldown reason
    /// as `next_phone`).
    fn next_email(&self) -> String {
        format!("user{}@example.com", self.email_seq.fetch_add(1, Ordering::SeqCst))
    }

    /// The single code captured for `email`, failing the test if none or
    /// several were sent (several would mean the fixture reused an address
    /// across a cooldown boundary).
    fn mail_code(&self, email: &str) -> String {
        let sent = self.mail_sent.lock().expect("capture lock poisoned");
        let matches: Vec<&String> = sent
            .iter()
            .filter(|(e, _)| e == email)
            .map(|(_, c)| c)
            .collect();
        assert_eq!(
            matches.len(),
            1,
            "expected exactly one code for {email}, got {}",
            matches.len()
        );
        matches[0].clone()
    }

    /// Provision a fresh account through the SMS flow, returning
    /// (user_id, token, phone).
    async fn provision_user_on(&self) -> (String, String, String) {
        let phone = self.next_phone();
        let (status, body) = self.sms_send_and_login(&phone).await;
        assert_eq!(status, StatusCode::OK, "provisioning {} failed: {:?}", phone, body);
        (
            body["user_id"].as_str().unwrap().to_string(),
            body["token"].as_str().unwrap().to_string(),
            phone,
        )
    }

    /// Provision a fresh account through the SMS flow, returning (user_id, token).
    async fn provision_user(&self) -> (String, String) {
        let (id, token, _) = self.provision_user_on().await;
        (id, token)
    }

    /// Provision a fresh account through the SMS flow and make it the fixture's
    /// current identity.
    async fn sms_login(&mut self) {
        let (_, token) = self.provision_user().await;
        self.token = Some(token);
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

    /// Seed the default admin account and log in through the bootstrap password.
    ///
    /// The returned token has no phone bound yet, so the forced-binding gate
    /// only lets it reach `/api/profile*`. Call `bind_phone_number` to unlock
    /// the rest of the API.
    async fn bootstrap_admin(&self) -> String {
        jcowork_gateway::auth::ensure_default_admin(&self.user_store)
            .await
            .expect("failed to seed default admin");
        let (status, resp) = self
            .login_raw(
                jcowork_gateway::auth::DEFAULT_ADMIN_USERNAME,
                jcowork_gateway::auth::DEFAULT_ADMIN_PASSWORD,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "admin bootstrap login should succeed");
        assert_eq!(resp["is_admin"], json!(true), "admin login should report is_admin");
        assert_eq!(
            resp["must_bind_phone"],
            json!(true),
            "bootstrap login must flag the account as needing a phone"
        );
        resp["token"].as_str().unwrap().to_string()
    }

    /// Bind a freshly generated number to the account holding `token`.
    /// Returns the number that was bound.
    async fn bind_phone_number(&self, token: &str) -> String {
        let phone = self.next_phone();
        let (status, body) = self
            .post_public("/api/auth/sms/send", json!({ "phone": phone }))
            .await;
        assert_eq!(status, StatusCode::OK, "sending a code to {} failed: {:?}", phone, body);
        let code = self.captured_code(&phone);

        let res = self
            .make_request_with_token(
                Some(token),
                "POST",
                "/api/profile/phone",
                Some(json!({ "phone": phone, "code": code })),
            )
            .await;
        assert_eq!(res.status(), StatusCode::OK, "binding {} should succeed", phone);
        phone
    }

    /// Seed the default admin, walk it through the bootstrap password door, and
    /// satisfy the forced phone binding — returning a token that can reach the
    /// whole API.
    async fn seed_and_login_admin(&self) -> String {
        let token = self.bootstrap_admin().await;
        self.bind_phone_number(&token).await;
        // The same JWT keeps working after binding: the middleware re-reads the
        // account on every request, so the gate opens as soon as a phone exists.
        token
    }

    /// POST to a public (unauthenticated) endpoint and return (status, body).
    async fn post_public(&self, path: &str, body: serde_json::Value) -> (StatusCode, serde_json::Value) {
        use tower::ServiceExt;

        let req = Request::builder()
            .method("POST")
            .uri(path)
            .header("Content-Type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();

        let res = self.router.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let body_bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let resp: serde_json::Value =
            serde_json::from_slice(&body_bytes).unwrap_or(serde_json::Value::Null);
        (status, resp)
    }

    /// Most recent code the fake channel captured for `phone`.
    fn captured_code(&self, phone: &str) -> String {
        self.sms_sent
            .lock()
            .expect("capture lock poisoned")
            .iter()
            .rev()
            .find(|(p, _)| p == phone)
            .map(|(_, c)| c.clone())
            .unwrap_or_else(|| panic!("no code was sent to {}", phone))
    }

    /// Number of messages the fake channel has been asked to deliver.
    fn captured_count(&self) -> usize {
        self.sms_sent.lock().expect("capture lock poisoned").len()
    }

    /// PUT a new display name for the account owning `token`.
    async fn rename(&self, token: &str, name: &str) -> axum::http::Response<Body> {
        self.make_request_with_token(
            Some(token),
            "PUT",
            "/api/profile/username",
            Some(json!({ "username": name })),
        )
        .await
    }

    /// Send a code to `phone` and immediately log in with it.
    /// Asserts the send succeeded and returns the login (status, body).
    async fn sms_send_and_login(&self, phone: &str) -> (StatusCode, serde_json::Value) {
        let (status, _) = self
            .post_public("/api/auth/sms/send", json!({ "phone": phone }))
            .await;
        assert_eq!(status, StatusCode::OK, "sending a code to {} should work", phone);
        let code = self.captured_code(phone);
        self.post_public("/api/auth/sms/login", json!({ "phone": phone, "code": code }))
            .await
    }
}

#[tokio::test]
async fn test_password_signup_endpoints_are_gone() {
    let app = TestApp::new().await;

    // SMS login is the only way in, so none of these may still resolve. The
    // router's GET-only SPA fallback answers unmatched paths, which turns a
    // deleted POST route into a 405 rather than a 404.
    for path in [
        "/api/auth/register",
        "/api/auth/forgot-password",
        "/api/auth/reset-password",
    ] {
        let (status, _) = app
            .post_public(path, json!({ "username": "x", "password": "y", "code": "z" }))
            .await;
        assert!(
            status == StatusCode::NOT_FOUND || status == StatusCode::METHOD_NOT_ALLOWED,
            "{} must not accept a signup request, got {}",
            path,
            status
        );
    }
}

#[tokio::test]
async fn test_password_login_is_admin_bootstrap_only() {
    let app = TestApp::new().await;
    jcowork_gateway::auth::ensure_default_admin(&app.user_store)
        .await
        .unwrap();

    let (status, _) = app.login_raw("admin", "not-the-password").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // A non-admin account is refused even when it holds the right password, and
    // the body is identical to a plain wrong-password failure — otherwise the
    // endpoint would disclose which usernames hold admin accounts.
    let hash =
        jcowork_gateway::auth::hash_password(jcowork_gateway::auth::DEFAULT_ADMIN_PASSWORD)
            .unwrap();
    app.user_store
        .create_user("normal", &hash, false)
        .await
        .unwrap();
    let (status, body) = app
        .login_raw("normal", jcowork_gateway::auth::DEFAULT_ADMIN_PASSWORD)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"], json!("Invalid username or password"));

    // The bootstrap door does admit the admin while it has no phone.
    let (status, body) = app
        .login_raw(
            jcowork_gateway::auth::DEFAULT_ADMIN_USERNAME,
            jcowork_gateway::auth::DEFAULT_ADMIN_PASSWORD,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{:?}", body);
    assert_eq!(body["must_bind_phone"], json!(true));
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
    app.sms_login().await;

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
    app.sms_login().await;

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
    app.sms_login().await;

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
    app.sms_login().await;

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
    app.sms_login().await;

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
    let (victim_id, victim_token) = app.provision_user().await;

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

    // The old token is rejected. That a trashed account also cannot log back in
    // over SMS is covered by `test_sms_login_rejects_trashed_account` — the
    // resend cooldown means this number cannot yield a second code here.
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
    let res = app
        .make_request_with_token(Some(&victim_token), "GET", "/api/providers", None)
        .await;
    assert_eq!(res.status(), StatusCode::OK, "old token must work again after restore");

    // Permanently delete the account; its token stops resolving to a user.
    // Logging in again would prove nothing here — the freed number would simply
    // auto-provision a brand-new account.
    let res = app
        .make_request_with_token(Some(&admin_token), "DELETE", &format!("/api/admin/users/{}", victim_id), None)
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    let res = app
        .make_request_with_token(Some(&victim_token), "GET", "/api/providers", None)
        .await;
    assert_eq!(
        res.status(),
        StatusCode::UNAUTHORIZED,
        "a permanently deleted account's token must be rejected"
    );
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

    // Provisioned accounts are always private now that self-service signup is
    // gone, so the admin has to publish the one this test expects to find.
    let (pub_id, _) = app.provision_user().await;
    let res = app
        .make_request_with_token(
            Some(&admin_token),
            "POST",
            &format!("/api/admin/users/{}/public", pub_id),
            Some(json!({ "is_public": true })),
        )
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    let (_, regular_token) = app.provision_user().await;

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

/// Admin can flip a user's public flag in both directions.
#[tokio::test]
async fn test_admin_toggles_user_public_flag() {
    let app = TestApp::new().await;
    let admin_token = app.seed_and_login_admin().await;

    // A private target user and an unrelated observer
    let (target_id, _, target_phone) = app.provision_user_on().await;
    let (_, observer_token) = app.provision_user().await;

    let public_url = format!("/api/admin/users/{}/public", target_id);

    // Non-admin callers are rejected
    let res = app
        .make_request_with_token(Some(&observer_token), "POST", &public_url, Some(json!({"is_public": true})))
        .await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);

    // Unknown user returns 404
    let res = app
        .make_request_with_token(Some(&admin_token), "POST", "/api/admin/users/does-not-exist/public", Some(json!({"is_public": true})))
        .await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    // Private user is invisible in the public directory…
    let is_listed = |app_body: &serde_json::Value| {
        app_body
            .as_array()
            .unwrap()
            .iter()
            .any(|u| u["user_id"] == json!(target_id))
    };
    let res = app
        .make_request_with_token(Some(&observer_token), "GET", "/api/public-users", None)
        .await;
    let body: serde_json::Value =
        serde_json::from_slice(&axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap())
            .unwrap();
    assert!(!is_listed(&body), "private user must not be in the public directory");

    // …until the admin makes it public
    let res = app
        .make_request_with_token(Some(&admin_token), "POST", &public_url, Some(json!({"is_public": true})))
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    let res = app
        .make_request_with_token(Some(&observer_token), "GET", "/api/public-users", None)
        .await;
    let body: serde_json::Value =
        serde_json::from_slice(&axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap())
            .unwrap();
    assert!(is_listed(&body), "public user must appear in the public directory");

    // The admin list reflects the updated flag, and finds the account by phone
    // number as well as by name
    let res = app
        .make_request_with_token(
            Some(&admin_token),
            "GET",
            &format!("/api/admin/users?query={}&status=active", target_phone),
            None,
        )
        .await;
    let body: serde_json::Value =
        serde_json::from_slice(&axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap())
            .unwrap();
    let row = body
        .as_array()
        .unwrap()
        .iter()
        .find(|u| u["user_id"] == json!(target_id))
        .unwrap();
    assert_eq!(row["is_public"], json!(true));

    // …and back to private
    let res = app
        .make_request_with_token(Some(&admin_token), "POST", &public_url, Some(json!({"is_public": false})))
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    let res = app
        .make_request_with_token(Some(&observer_token), "GET", "/api/public-users", None)
        .await;
    let body: serde_json::Value =
        serde_json::from_slice(&axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap())
            .unwrap();
    assert!(!is_listed(&body), "user made private must leave the public directory");

    // The target's stored flag matches the store as well
    let stored = app.user_store.get_user_by_id(&target_id).await.unwrap().unwrap();
    assert!(!stored.is_public);
}

// ── SMS verification-code login ──
//
// The 60-second per-phone resend cooldown means a test can only ever obtain one
// code per phone number, so each case below uses a distinct number whenever it
// needs to send twice.

#[tokio::test]
async fn test_sms_login_provisions_account_and_returns_working_token() {
    let app = TestApp::new().await;
    let phone = "13800138001";

    let (status, body) = app.sms_send_and_login(phone).await;
    assert_eq!(status, StatusCode::OK, "sms login should succeed: {:?}", body);
    assert!(!body["token"].as_str().unwrap().is_empty());
    assert_eq!(body["is_admin"], json!(false));
    // The account was created from this number, so it is already bound.
    assert_eq!(body["must_bind_phone"], json!(false));
    // The display name is derived from the last four digits only.
    assert_eq!(body["username"], json!("用户8001"));

    let token = body["token"].as_str().unwrap();
    let res = app
        .make_request_with_token(Some(token), "GET", "/api/profile", None)
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    let profile: serde_json::Value =
        serde_json::from_slice(&axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap())
            .unwrap();
    assert_eq!(profile["phone_bound"], json!(true));
    assert_eq!(profile["phone_masked"], json!("*******8001"));
    // The full number must never come back to the client.
    assert!(!profile.to_string().contains(phone));
}

#[tokio::test]
async fn test_sms_send_response_never_leaks_the_code() {
    let app = TestApp::new().await;
    let phone = "13800138002";

    let (status, body) = app
        .post_public("/api/auth/sms/send", json!({ "phone": phone }))
        .await;
    assert_eq!(status, StatusCode::OK);

    // The code exists only in the server log. Echoing it — or naming the channel
    // that produced it — would tell an attacker exactly where to look.
    let text = body.to_string();
    let code = app.captured_code(phone);
    assert!(!code.is_empty());
    assert!(!text.contains(&code), "response leaked the code: {}", text);
    assert!(body.get("code").is_none(), "response has a `code` field: {}", text);
    assert!(!text.contains("dev"), "response leaked the provider name: {}", text);
    assert!(!text.contains("capture"), "response leaked the provider name: {}", text);
    // Only the timings the client needs to drive its countdown.
    assert_eq!(body["expires_in"], json!(300));
    assert_eq!(body["resend_after"], json!(60));
}

#[tokio::test]
async fn test_sms_rejects_invalid_phone_numbers() {
    let app = TestApp::new().await;

    for bad in ["12345", "23800138000", "138001380001", "not-a-phone", ""] {
        let (status, _) = app
            .post_public("/api/auth/sms/send", json!({ "phone": bad }))
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{:?} should be rejected", bad);
    }
    assert_eq!(app.captured_count(), 0, "no message should reach the carrier");

    let (status, _) = app
        .post_public("/api/auth/sms/login", json!({ "phone": "12345", "code": "000000" }))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_sms_code_is_single_use_and_rejected_when_wrong() {
    let app = TestApp::new().await;
    let phone = "13800138003";

    let (status, _) = app
        .post_public("/api/auth/sms/send", json!({ "phone": phone }))
        .await;
    assert_eq!(status, StatusCode::OK);
    let code = app.captured_code(phone);

    let (status, body) = app
        .post_public("/api/auth/sms/login", json!({ "phone": phone, "code": "000000" }))
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"], json!("Invalid verification code"));
    assert_eq!(body["attempts_left"], json!(4));

    let (status, _) = app
        .post_public("/api/auth/sms/login", json!({ "phone": phone, "code": code }))
        .await;
    assert_eq!(status, StatusCode::OK);

    // Replaying the spent code must not mint a second session.
    let (status, _) = app
        .post_public("/api/auth/sms/login", json!({ "phone": phone, "code": code }))
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "a spent code must not log in again");

    // Nor may it be used against a number that never requested one.
    let (status, _) = app
        .post_public("/api/auth/sms/login", json!({ "phone": "13800138099", "code": code }))
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_sms_locks_out_after_repeated_wrong_codes() {
    let app = TestApp::new().await;
    let phone = "13800138004";

    let (status, _) = app
        .post_public("/api/auth/sms/send", json!({ "phone": phone }))
        .await;
    assert_eq!(status, StatusCode::OK);
    let code = app.captured_code(phone);

    for expected_left in (0..5).rev() {
        let (status, body) = app
            .post_public("/api/auth/sms/login", json!({ "phone": phone, "code": "000000" }))
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["attempts_left"], json!(expected_left));
    }

    // The sixth attempt trips the lock even with the correct code — that is what
    // keeps a 6-digit number from being brute-forceable inside the 5-minute TTL.
    let (status, body) = app
        .post_public("/api/auth/sms/login", json!({ "phone": phone, "code": code }))
        .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert!(body["retry_after"].as_i64().unwrap() > 0);

    let (status, _) = app
        .post_public("/api/auth/sms/login", json!({ "phone": phone, "code": code }))
        .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn test_sms_resend_cooldown() {
    let app = TestApp::new().await;
    let phone = "13800138005";

    let (status, _) = app
        .post_public("/api/auth/sms/send", json!({ "phone": phone }))
        .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = app
        .post_public("/api/auth/sms/send", json!({ "phone": phone }))
        .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert!(body["retry_after"].as_i64().unwrap() > 0);
    assert_eq!(app.captured_count(), 1, "a blocked resend must not reach the carrier");

    // A different number is unaffected.
    let (status, _) = app
        .post_public("/api/auth/sms/send", json!({ "phone": "13800138006" }))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(app.captured_count(), 2);
}

#[tokio::test]
async fn test_sms_daily_global_cap_stops_toll_fraud() {
    let config = jcowork_sms::config::SmsConfig {
        daily_global_cap: 3,
        ..jcowork_sms::config::SmsConfig::default()
    };
    let app = TestApp::with_sms(config).await;

    // Distinct numbers, so only the process-wide budget can stop these.
    for i in 0..3 {
        let (status, _) = app
            .post_public("/api/auth/sms/send", json!({ "phone": format!("1390013800{}", i) }))
            .await;
        assert_eq!(status, StatusCode::OK, "send {} should be within budget", i);
    }
    assert_eq!(app.captured_count(), 3);

    let (status, body) = app
        .post_public("/api/auth/sms/send", json!({ "phone": "13900138009" }))
        .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body["error"], json!("SMS quota exhausted, please try again tomorrow"));
    assert_eq!(app.captured_count(), 3, "the cap must be enforced before the carrier call");
}

#[tokio::test]
async fn test_sms_invite_only_mode_refuses_unknown_numbers() {
    let config = jcowork_sms::config::SmsConfig {
        allow_register: false,
        ..jcowork_sms::config::SmsConfig::default()
    };
    let app = TestApp::with_sms(config).await;

    let unknown = "13800138007";
    let (status, body) = app.sms_send_and_login(unknown).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], json!("Phone number is not registered"));
    assert!(
        app.user_store.get_user_by_phone(unknown).await.unwrap().is_none(),
        "invite-only mode must not provision an account"
    );

    // A number an admin has assigned can still log in.
    let assigned = "13800138017";
    let seeded = app
        .user_store
        .create_user("seeded-user", "irrelevant", false)
        .await
        .unwrap();
    app.user_store.set_user_phone(&seeded.id, assigned).await.unwrap();

    let (status, body) = app.sms_send_and_login(assigned).await;
    assert_eq!(status, StatusCode::OK, "{:?}", body);
    assert_eq!(body["user_id"], json!(seeded.id));
}

#[tokio::test]
async fn test_sms_login_rejects_trashed_account() {
    let app = TestApp::new().await;
    let phone = "13800138008";

    let user = app.user_store.create_user("trash-me", "irrelevant", false).await.unwrap();
    app.user_store.set_user_phone(&user.id, phone).await.unwrap();
    app.user_store.soft_delete_user(&user.id).await.unwrap();

    // The number is still occupied by the trashed row, so the lookup finds it and
    // the handler rejects instead of falling through to a second INSERT that
    // would hit the unique index and surface as an opaque 500.
    let (status, body) = app.sms_send_and_login(phone).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], json!("Account has been deleted"));
}

#[tokio::test]
async fn test_bind_phone_closes_the_password_door() {
    let app = TestApp::new().await;
    let phone = "13800138010";

    // Deliberately stop short of binding: this test performs the bind itself.
    let admin_token = app.bootstrap_admin().await;

    // The bootstrap password works while no phone is bound.
    let (status, _) = app
        .login_raw(
            jcowork_gateway::auth::DEFAULT_ADMIN_USERNAME,
            jcowork_gateway::auth::DEFAULT_ADMIN_PASSWORD,
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    let (status, _) = app
        .post_public("/api/auth/sms/send", json!({ "phone": phone }))
        .await;
    assert_eq!(status, StatusCode::OK);
    let code = app.captured_code(phone);

    let res = app
        .make_request_with_token(
            Some(&admin_token),
            "POST",
            "/api/profile/phone",
            Some(json!({ "phone": phone, "code": code })),
        )
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    let profile: serde_json::Value =
        serde_json::from_slice(&axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap())
            .unwrap();
    assert_eq!(profile["phone_bound"], json!(true));
    assert_eq!(profile["is_admin"], json!(true));
    assert_eq!(profile["phone_masked"], json!("*******8010"));

    // The default admin password is a published constant, so binding has to
    // retire it — otherwise anyone who can reach the port keeps a way in.
    let (status, _) = app
        .login_raw(
            jcowork_gateway::auth::DEFAULT_ADMIN_USERNAME,
            jcowork_gateway::auth::DEFAULT_ADMIN_PASSWORD,
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "password login must fail once a phone is bound");

    // A token minted during bootstrap stays valid until it expires; there is no
    // token-version column to revoke it with. This is a known, documented limit.
    let res = app
        .make_request_with_token(Some(&admin_token), "GET", "/api/profile", None)
        .await;
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_profile_requires_auth_and_validates_username() {
    let app = TestApp::new().await;

    let res = app.make_request_with_token(None, "GET", "/api/profile", None).await;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    let (status, body) = app.sms_send_and_login("13800138011").await;
    assert_eq!(status, StatusCode::OK);
    let token = body["token"].as_str().unwrap().to_string();

    // Too short, too long, and characters that would break `@`-mentions or
    // markdown rendering.
    for bad in [
        "a",
        "way-too-long-a-display-name-that-exceeds-the-limit",
        "a@b",
        "a b",
        "a/b",
        "a\\b",
    ] {
        let res = app.rename(&token, bad).await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{:?} should be rejected", bad);
    }

    let res = app.rename(&token, "新名字").await;
    assert_eq!(res.status(), StatusCode::OK);
    let profile: serde_json::Value =
        serde_json::from_slice(&axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap())
            .unwrap();
    assert_eq!(profile["username"], json!("新名字"));

    // A name that is already taken is a conflict, not a server error.
    let (status, body2) = app.sms_send_and_login("13800138012").await;
    assert_eq!(status, StatusCode::OK);
    let token2 = body2["token"].as_str().unwrap().to_string();
    let res = app.rename(&token2, "新名字").await;
    assert_eq!(res.status(), StatusCode::CONFLICT);
}

async fn json_body(res: axum::http::Response<Body>) -> serde_json::Value {
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

// ── Forced phone binding ──

/// An account with no phone bound may only reach the endpoints the binding
/// screen needs; everything else is refused until the number exists.
#[tokio::test]
async fn test_unbound_account_is_locked_to_the_binding_endpoints() {
    let app = TestApp::new().await;
    let token = app.bootstrap_admin().await;

    let res = app
        .make_request_with_token(Some(&token), "GET", "/api/profile", None)
        .await;
    assert_eq!(
        res.status(),
        StatusCode::OK,
        "the binding screen must be able to read the profile"
    );

    for path in ["/api/providers", "/api/sessions", "/api/admin/users"] {
        let res = app
            .make_request_with_token(Some(&token), "GET", path, None)
            .await;
        assert_eq!(res.status(), StatusCode::FORBIDDEN, "{} must be gated", path);
        // The frontend branches on this code to redirect, so it has to be there.
        assert_eq!(json_body(res).await["code"], json!("phone_bind_required"));
    }

    // Binding lifts the gate for the very same token.
    app.bind_phone_number(&token).await;
    let res = app
        .make_request_with_token(Some(&token), "GET", "/api/providers", None)
        .await;
    assert_eq!(
        res.status(),
        StatusCode::OK,
        "binding a phone must unlock the rest of the API"
    );
}

// ── Admin phone assignment ──

/// Assigning a number is the only way back into an account that predates SMS
/// login, so it has to work end to end and retire the account's password.
#[tokio::test]
async fn test_admin_assigns_and_removes_a_phone_number() {
    let app = TestApp::new().await;
    let admin_token = app.seed_and_login_admin().await;

    // A legacy account: no number, so nothing it holds can authenticate.
    let hash = jcowork_gateway::auth::hash_password("legacypass1").unwrap();
    let legacy = app
        .user_store
        .create_user("legacy", &hash, false)
        .await
        .unwrap();
    assert!(legacy.phone.is_none());

    let phone = app.next_phone();
    let url = format!("/api/admin/users/{}/phone", legacy.id);

    // Only an admin may assign numbers.
    let (_, other_token) = app.provision_user().await;
    let res = app
        .make_request_with_token(Some(&other_token), "POST", &url, Some(json!({ "phone": phone })))
        .await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);

    let res = app
        .make_request_with_token(
            Some(&admin_token),
            "POST",
            &url,
            Some(json!({ "phone": "12345" })),
        )
        .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST, "an invalid number must be rejected");

    let res = app
        .make_request_with_token(Some(&admin_token), "POST", &url, Some(json!({ "phone": phone })))
        .await;
    assert_eq!(res.status(), StatusCode::OK);

    // The number now authenticates that same account, not a new one.
    let (status, body) = app.sms_send_and_login(&phone).await;
    assert_eq!(status, StatusCode::OK, "{:?}", body);
    assert_eq!(body["user_id"], json!(legacy.id));

    // Assigning stamps the SMS-only sentinel over the stored hash.
    let stored = app.user_store.get_user_by_id(&legacy.id).await.unwrap().unwrap();
    assert!(
        !jcowork_gateway::auth::verify_password("legacypass1", &stored.password_hash)
            .unwrap_or(false),
        "assigning a phone must retire the account's password"
    );
    let (status, _) = app.login_raw("legacy", "legacypass1").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // The admin list masks the number and can search by it.
    let res = app
        .make_request_with_token(
            Some(&admin_token),
            "GET",
            &format!("/api/admin/users?query={}&status=active", phone),
            None,
        )
        .await;
    let body = json_body(res).await;
    let row = body
        .as_array()
        .unwrap()
        .iter()
        .find(|u| u["user_id"] == json!(legacy.id))
        .expect("the assigned number must make the account searchable");
    assert_eq!(row["phone_bound"], json!(true));
    assert_eq!(row["phone_masked"], json!(format!("*******{}", &phone[7..])));
    assert!(row.get("password_hash").is_none());

    // Unbinding frees the number; doing it twice is a conflict.
    let res = app
        .make_request_with_token(Some(&admin_token), "DELETE", &url, None)
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    let res = app
        .make_request_with_token(Some(&admin_token), "DELETE", &url, None)
        .await;
    assert_eq!(res.status(), StatusCode::CONFLICT);

    // The admin cannot unbind itself: with no number and an unparseable hash
    // there would be no way back in short of a restart with JCWORK_ADMIN_PHONE.
    let res = app
        .make_request_with_token(Some(&admin_token), "GET", "/api/admin/users?status=active", None)
        .await;
    let body = json_body(res).await;
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
            "DELETE",
            &format!("/api/admin/users/{}/phone", admin_id),
            None,
        )
        .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

// ── Email verification login ──

/// The send response tells the client when the code expires and when a resend
/// is allowed — and nothing else. The code itself travels only through the
/// mail channel, so it must not appear anywhere in the HTTP body.
#[tokio::test]
async fn test_email_send_response_never_leaks_the_code() {
    let app = TestApp::new().await;
    let email = app.next_email();

    let (status, body) = app.post_public("/api/auth/email/send", json!({ "email": email })).await;
    assert_eq!(status, StatusCode::OK, "{:?}", body);
    assert_eq!(body["expires_in"], json!(300));
    assert!(body["resend_after"].as_i64().unwrap() > 0);

    let code = app.mail_code(&email);
    let raw = body.to_string();
    assert!(
        !raw.contains(&code),
        "the mailed code must not appear in the send response"
    );

    // Malformed addresses are refused before anything is staged or sent.
    for bad in ["", "no-at-sign", "a@b", "user@@example.com", "user@.example.com"] {
        let (status, body) = app.post_public("/api/auth/email/send", json!({ "email": bad })).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad:?} should be rejected: {:?}", body);
        assert_eq!(body["error"], json!("Invalid email address"));
        assert_eq!(app.mail_sent.lock().unwrap().len(), 1, "nothing more may be sent");
    }
}

/// Register → token works despite having no phone (the gate keys on
/// phone-or-email) → password login in both directions → duplicate
/// registration is a conflict, case-insensitively.
#[tokio::test]
async fn test_email_register_login_and_gate_passes_without_phone() {
    let app = TestApp::new().await;
    let email = app.next_email();
    let password = "initial-pass-1";

    let (status, _) = app.post_public("/api/auth/email/send", json!({ "email": email })).await;
    assert_eq!(status, StatusCode::OK);
    let code = app.mail_code(&email);

    // Cheap rejections happen before the code is spent.
    let (status, body) = app
        .post_public(
            "/api/auth/email/register",
            json!({ "email": email, "password": "short", "code": code }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{:?}", body);
    assert_eq!(body["error"], json!("Password must be at least 8 characters"));

    let (status, body) = app
        .post_public(
            "/api/auth/email/register",
            json!({ "email": email, "password": password, "code": code }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{:?}", body);
    assert_eq!(body["is_admin"], json!(false));
    assert_eq!(
        body["must_bind_phone"],
        json!(false),
        "an account born with a bound email must never see the binding screen"
    );
    let user_id = body["user_id"].as_str().unwrap().to_string();
    let username = body["username"].as_str().unwrap().to_string();
    assert!(
        username.starts_with("user"),
        "username is derived from the local part: got {username}"
    );

    // The token reaches normal endpoints even though the account has no phone.
    let token = body["token"].as_str().unwrap().to_string();
    let res = app.make_request_with_token(Some(&token), "GET", "/api/skills", None).await;
    assert_eq!(
        res.status(),
        StatusCode::OK,
        "an email-only account must not be stopped by the phone-binding gate"
    );
    let res = app.make_request_with_token(Some(&token), "GET", "/api/profile", None).await;
    let profile = json_body(res).await;
    assert_eq!(profile["email_bound"], json!(true));
    assert_eq!(profile["phone_bound"], json!(false));
    assert_eq!(profile["email_masked"], json!("****@example.com"));

    // Password login works, and upper-casing the address still finds the
    // account because the server normalizes before lookup.
    let (status, body) = app
        .post_public(
            "/api/auth/email/login",
            json!({ "email": email.to_uppercase(), "password": password }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{:?}", body);
    assert_eq!(body["user_id"], json!(user_id));
    assert_eq!(body["must_bind_phone"], json!(false));

    let (status, body) = app
        .post_public(
            "/api/auth/email/login",
            json!({ "email": email, "password": "wrong-password" }),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"], json!("Invalid email or password"));

    // Unknown addresses fail exactly like wrong passwords.
    let (status, body) = app
        .post_public(
            "/api/auth/email/login",
            json!({ "email": app.next_email(), "password": password }),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"], json!("Invalid email or password"));

    // Re-registering the address is a conflict — whatever code is offered, the
    // duplicate check runs before the code is consumed.
    let (status, body) = app
        .post_public(
            "/api/auth/email/register",
            json!({ "email": email, "password": "another-pass-2", "code": "000000" }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{:?}", body);
    assert_eq!(body["error"], json!("Email address is already registered"));
    // ...and the case-insensitive spelling is the same address.
    let (status, _) = app
        .post_public(
            "/api/auth/email/register",
            json!({ "email": email.to_uppercase(), "password": "another-pass-2", "code": "000000" }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

/// Wrong codes count attempts before the right one succeeds, and a consumed
/// code never verifies a second time.
#[tokio::test]
async fn test_email_code_wrong_then_correct_and_single_use() {
    let app = TestApp::new().await;
    let email = app.next_email();

    let (status, _) = app.post_public("/api/auth/email/send", json!({ "email": email })).await;
    assert_eq!(status, StatusCode::OK);
    let code = app.mail_code(&email);

    let (status, body) = app
        .post_public(
            "/api/auth/email/register",
            json!({ "email": email, "password": "some-pass-123", "code": "000000" }),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{:?}", body);
    assert_eq!(body["error"], json!("Invalid verification code"));
    assert_eq!(body["attempts_left"], json!(4));

    let (status, body) = app
        .post_public(
            "/api/auth/email/register",
            json!({ "email": email, "password": "some-pass-123", "code": code }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{:?}", body);

    // The code is spent: password reset may not reuse it.
    let (status, body) = app
        .post_public(
            "/api/auth/password/reset",
            json!({ "email": email, "code": code, "new_password": "fresh-pass-45" }),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{:?}", body);
    assert_eq!(body["error"], json!("Verification code has expired, please request a new one"));
}

/// Forgot password: a mailed code unlocks setting a new one, after which only
/// the new password authenticates.
#[tokio::test]
async fn test_email_reset_password_flow() {
    let app = TestApp::new().await;
    let email = app.next_email();

    // Seeded directly so the address has never been mailed to: the 60-second
    // resend cooldown would otherwise block the test's own send.
    let old_hash = jcowork_gateway::auth::hash_password("old-pass-123").unwrap();
    app.user_store.create_email_user(&email, &old_hash).await.unwrap();

    let (status, body) = app.post_public("/api/auth/email/send", json!({ "email": email })).await;
    assert_eq!(status, StatusCode::OK, "{:?}", body);
    let code = app.mail_code(&email);

    // A too-short new password is refused without spending the code.
    let (status, body) = app
        .post_public(
            "/api/auth/password/reset",
            json!({ "email": email, "code": code, "new_password": "short" }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{:?}", body);

    let (status, body) = app
        .post_public(
            "/api/auth/password/reset",
            json!({ "email": email, "code": code, "new_password": "fresh-pass-45" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{:?}", body);

    let (status, body) = app
        .post_public(
            "/api/auth/email/login",
            json!({ "email": email, "password": "fresh-pass-45" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "the new password must authenticate: {:?}", body);

    let (status, body) = app
        .post_public(
            "/api/auth/email/login",
            json!({ "email": email, "password": "old-pass-123" }),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "the old password must be dead: {:?}", body);
}

/// The send endpoint answers registered and unregistered addresses alike
/// (anti-enumeration); only consuming a code — proof of mailbox control —
/// earns a 404 from reset.
#[tokio::test]
async fn test_email_reset_unknown_address_is_not_found() {
    let app = TestApp::new().await;
    let email = app.next_email();

    let (status, body) = app.post_public("/api/auth/email/send", json!({ "email": email })).await;
    assert_eq!(status, StatusCode::OK, "send must not reveal whether the address holds an account: {:?}", body);
    let code = app.mail_code(&email);

    let (status, body) = app
        .post_public(
            "/api/auth/password/reset",
            json!({ "email": email, "code": code, "new_password": "some-pass-67" }),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{:?}", body);
    assert_eq!(body["error"], json!("Account not found"));
}

/// `JCWORK_MAIL_ALLOW_REGISTER=false` turns the mailbox into a recovery-only
/// channel: codes still go out, but registration is refused outright.
#[tokio::test]
async fn test_email_registration_can_be_disabled() {
    let app = TestApp::with_email_registration(false).await;
    let email = app.next_email();

    let (status, _) = app.post_public("/api/auth/email/send", json!({ "email": email })).await;
    assert_eq!(status, StatusCode::OK, "reset still needs codes to flow");
    let code = app.mail_code(&email);

    let (status, body) = app
        .post_public(
            "/api/auth/email/register",
            json!({ "email": email, "password": "some-pass-89", "code": code }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{:?}", body);
    assert_eq!(body["error"], json!("Email registration is disabled"));
}
