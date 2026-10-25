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
    summary::SummaryService,
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
    /// Full messages (registration credentials), as (email, subject, body).
    messages: Arc<std::sync::Mutex<Vec<(String, String, String)>>>,
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

    async fn send_message(
        &self,
        email: &str,
        subject: &str,
        body: &str,
    ) -> anyhow::Result<()> {
        self.messages
            .lock()
            .expect("capture lock poisoned")
            .push((email.to_string(), subject.to_string(), body.to_string()));
        Ok(())
    }

    fn name(&self) -> &'static str {
        "capture"
    }
}

/// Deterministic captcha generator: records every `(id, code)` it issues so
/// tests can complete a registration without reading the rendered image.
/// Same rule as `CaptureSms`/`CaptureMail`: the fake lives in the test binary
/// only — a readable code path in production code would defeat the captcha.
struct RecordingCaptcha {
    issued: Arc<std::sync::Mutex<Vec<(String, String)>>>,
}

impl router::CaptchaProvider for RecordingCaptcha {
    fn generate(&self) -> (String, String, String) {
        let mut issued = self.issued.lock().expect("capture lock poisoned");
        let id = format!("cap-{}", issued.len());
        let code = format!("CAP{:04}", issued.len());
        issued.push((id.clone(), code.clone()));
        (id, code, "data:image/png;base64,fake".to_string())
    }
}

/// Test fixture that sets up a minimal gateway with in-memory SQLite
struct TestApp {
    _temp_dir: TempDir,
    router: Router,
    token: Option<String>,
    /// Per-user data root (workspaces + index DBs live under here).
    data_dir: String,
    /// Shared handle to the user store (for seeding/purge assertions).
    user_store: Arc<UserStore>,
    /// Codes captured from the fake SMS channel, as (phone, code) pairs.
    sms_sent: Arc<std::sync::Mutex<Vec<(String, String)>>>,
    /// Codes captured from the fake mail channel, as (email, code) pairs.
    mail_sent: Arc<std::sync::Mutex<Vec<(String, String)>>>,
    /// Full messages captured from the fake mail channel (registration
    /// credentials), as (email, subject, body) triples.
    mail_messages: Arc<std::sync::Mutex<Vec<(String, String, String)>>>,
    /// Captchas issued by the recording provider, as (id, code) pairs.
    captcha_issued: Arc<std::sync::Mutex<Vec<(String, String)>>>,
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
        let mail_capture = CaptureMail::default();
        let (mail_sent, mail_messages) = (mail_capture.sent.clone(), mail_capture.messages.clone());
        let mail = router::MailRuntime::new(Arc::new(mail_capture), 500, true);
        Self::build(config, mail, mail_sent, mail_messages).await
    }

    /// Build the test app with email registration turned off
    /// (`JCWORK_MAIL_ALLOW_REGISTER=false`).
    async fn with_email_registration(allow: bool) -> Self {
        let mail_capture = CaptureMail::default();
        let (mail_sent, mail_messages) = (mail_capture.sent.clone(), mail_capture.messages.clone());
        let mail = router::MailRuntime::new(Arc::new(mail_capture), 500, allow);
        Self::build(jcowork_sms::config::SmsConfig::default(), mail, mail_sent, mail_messages).await
    }

    /// A test app whose mail channel sits on the unconfigured auto→dev
    /// fallback — the state a fresh deployment (server or desktop) boots in
    /// before the admin configures SMTP, and the trigger for the admin
    /// mail-setup gate. Uses the real dev sender, so nothing is captured and
    /// `mail_sent` stays empty.
    async fn with_mail_auto_dev() -> Self {
        let mail = router::MailRuntime::from_config(&jcowork_mail::config::MailConfig::default())
            .expect("the default mail config must build");
        let empty_codes: Arc<std::sync::Mutex<Vec<(String, String)>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let empty_messages: Arc<std::sync::Mutex<Vec<(String, String, String)>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        Self::build(jcowork_sms::config::SmsConfig::default(), mail, empty_codes, empty_messages)
            .await
    }

    async fn build(
        config: jcowork_sms::config::SmsConfig,
        mail: router::MailRuntime,
        mail_sent: Arc<std::sync::Mutex<Vec<(String, String)>>>,
        mail_messages: Arc<std::sync::Mutex<Vec<(String, String, String)>>>,
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
        let llm_router = Arc::new(RwLock::new(LlmRouter::from_mock(mock_provider)));
        let summary = Arc::new(SummaryService::new(
            llm_router.clone(),
            "mock:test-model".to_string(),
        ));

        let user_store_handle = user_store.clone();

        let capture = CaptureSms::default();
        let sms_sent = capture.sent.clone();
        let sms = router::SmsRuntime::new(
            Arc::new(capture),
            config.daily_global_cap,
            config.allow_register,
        );

        let captcha_issued: Arc<std::sync::Mutex<Vec<(String, String)>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let captcha = router::CaptchaService::with_provider(Arc::new(RecordingCaptcha {
            issued: captcha_issued.clone(),
        }));

        let state = AppState {
            session_manager,
            auth_config: AuthConfig {
                jwt_secret: "test-secret".to_string(),
                token_duration_hours: 24,
            },
            llm_router,
            default_model: "mock:test-model".to_string(),
            cron_scheduler: {
                // Same wiring as the server binary: the scheduler persists
                // jobs/results through the shared SQLite pool.
                let mut scheduler = jcowork_cron::CronScheduler::new();
                scheduler.with_store(Arc::new(jcowork_storage::CronJobStore::new(pool.clone())));
                Arc::new(scheduler)
            },
            memory_manager,
            skill_manager,
            tool_registry,
            connector_manager,
            user_store,
            log_writer,
            app_settings: Arc::new(jcowork_storage::AppSettingsStore::new(pool.clone())),
            captcha: Arc::new(captcha),
            summary,
            feishu_config_store,
            feishu_client_cache: Arc::new(dashmap::DashMap::new()),
            sms,
            mail: Arc::new(RwLock::new(mail)),
            data_dir: data_dir.clone(),
        };

        let router = router::build_router(state);

        Self {
            _temp_dir: temp_dir,
            router,
            token: None,
            data_dir,
            user_store: user_store_handle,
            sms_sent,
            mail_sent,
            mail_messages,
            captcha_issued,
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

    /// Fetch a fresh image captcha through the public endpoint and return its
    /// (id, code) as recorded by the fake provider — the code itself never
    /// leaves the server, so tests can only read it from the recording.
    async fn fetch_captcha(&self) -> (String, String) {
        let res = self
            .make_request_with_token(None, "GET", "/api/auth/captcha", None)
            .await;
        assert_eq!(res.status(), StatusCode::OK);
        let body = json_body(res).await;
        let id = body["captcha_id"].as_str().expect("captcha_id").to_string();
        assert!(
            body["image"].as_str().unwrap_or("").starts_with("data:"),
            "the image must be a data URL: {:?}",
            body["image"]
        );
        assert_eq!(body["expires_in"], json!(300));
        let issued = self.captcha_issued.lock().expect("capture lock poisoned");
        let (_, code) = issued
            .iter()
            .rev()
            .find(|(i, _)| i == &id)
            .expect("the endpoint must have used the recording provider");
        (id, code.clone())
    }

    /// Register an email account through the public flow: fetch a captcha,
    /// then submit it with the form fields. Returns (status, body).
    async fn register_email(
        &self,
        username: &str,
        email: &str,
        password: &str,
    ) -> (StatusCode, serde_json::Value) {
        let (captcha_id, captcha_code) = self.fetch_captcha().await;
        self.post_public(
            "/api/auth/email/register",
            json!({
                "username": username,
                "email": email,
                "password": password,
                "captcha_id": captcha_id,
                "captcha_code": captcha_code,
            }),
        )
        .await
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
    /// The returned token has not replaced the bootstrap password yet, so the
    /// bootstrap gate only lets it reach `/api/profile` and
    /// `/api/profile/password`. Call `change_admin_password` to unlock the
    /// rest of the API.
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
            resp["must_change_password"],
            json!(true),
            "bootstrap login must flag the account as needing a new password"
        );
        resp["token"].as_str().unwrap().to_string()
    }

    /// Replace the bootstrap password on the account holding `token`.
    /// Returns the new password.
    async fn change_admin_password(&self, token: &str) -> String {
        const NEW_PASSWORD: &str = "replaced-admin-pass";
        let res = self
            .make_request_with_token(
                Some(token),
                "POST",
                "/api/profile/password",
                Some(json!({ "new_password": NEW_PASSWORD })),
            )
            .await;
        assert_eq!(res.status(), StatusCode::OK, "changing the admin password should succeed");
        NEW_PASSWORD.to_string()
    }

    /// Seed the default admin, walk it through the bootstrap password door, and
    /// replace the bootstrap password — returning a token that can reach the
    /// whole API.
    async fn seed_and_login_admin(&self) -> String {
        let token = self.bootstrap_admin().await;
        self.change_admin_password(&token).await;
        // The same JWT keeps working after the change: the middleware re-reads
        // the account on every request, so the gate opens as soon as the
        // password is no longer the bootstrap one.
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

    // The bootstrap door admits the admin while the bootstrap password is in
    // place, and flags the forced change.
    let (status, body) = app
        .login_raw(
            jcowork_gateway::auth::DEFAULT_ADMIN_USERNAME,
            jcowork_gateway::auth::DEFAULT_ADMIN_PASSWORD,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{:?}", body);
    assert_eq!(body["must_change_password"], json!(true));

    // After the change the self-chosen password is the permanent login.
    let token = body["token"].as_str().unwrap();
    let new_password = app.change_admin_password(token).await;
    let (status, body) = app
        .login_raw(jcowork_gateway::auth::DEFAULT_ADMIN_USERNAME, &new_password)
        .await;
    assert_eq!(status, StatusCode::OK, "{:?}", body);
    assert_eq!(body["must_change_password"], json!(false));

    // A bound phone no longer closes the password door.
    let admin = app
        .user_store
        .get_user_by_username(jcowork_gateway::auth::DEFAULT_ADMIN_USERNAME)
        .await
        .unwrap()
        .unwrap();
    let phone = app.next_phone();
    app.user_store.set_user_phone(&admin.id, &phone).await.unwrap();
    let (status, body) = app
        .login_raw(jcowork_gateway::auth::DEFAULT_ADMIN_USERNAME, &new_password)
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a phone-bound admin must still be able to use password login: {:?}",
        body
    );
}

/// A pre-upgrade admin whose password was sentinel-stamped (phone-bound,
/// SMS-only) fails closed at password login, is flagged at SMS login, and is
/// fully restored once the forced change sets a real password.
#[tokio::test]
async fn test_sentinel_admin_is_migrated_through_forced_password_change() {
    let app = TestApp::new().await;

    // Recreate the pre-upgrade state by seeding the admin directly: sentinel
    // hash (unparseable by Argon2), a bound phone, and no self-set password.
    // `ensure_admin_user` leaves `password_changed_at` NULL, unlike
    // `update_password_hash`, which is exactly what the era looked like.
    app.user_store
        .ensure_admin_user(
            jcowork_gateway::auth::DEFAULT_ADMIN_USERNAME,
            &jcowork_storage::sms_only_password_hash(),
        )
        .await
        .unwrap();
    let admin = app
        .user_store
        .get_user_by_username(jcowork_gateway::auth::DEFAULT_ADMIN_USERNAME)
        .await
        .unwrap()
        .unwrap();
    let phone = app.next_phone();
    app.user_store.set_user_phone(&admin.id, &phone).await.unwrap();

    // Password login fails closed on the unparseable hash — same body as any
    // other wrong password, so nothing about the account leaks.
    let (status, body) = app
        .login_raw(jcowork_gateway::auth::DEFAULT_ADMIN_USERNAME, "whatever-they-type")
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"], json!("Invalid username or password"));

    // SMS login works and flags the forced change.
    let (status, body) = app.sms_send_and_login(&phone).await;
    assert_eq!(status, StatusCode::OK, "{:?}", body);
    assert_eq!(body["is_admin"], json!(true));
    assert_eq!(body["must_change_password"], json!(true));

    // Until the change happens the token is locked to the profile endpoints…
    let token = body["token"].as_str().unwrap();
    let res = app
        .make_request_with_token(Some(token), "GET", "/api/providers", None)
        .await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);

    // …and one change restores password login permanently.
    let new_password = app.change_admin_password(token).await;
    let (status, body) = app
        .login_raw(jcowork_gateway::auth::DEFAULT_ADMIN_USERNAME, &new_password)
        .await;
    assert_eq!(status, StatusCode::OK, "{:?}", body);
    assert_eq!(body["must_change_password"], json!(false));
}

/// The change-password endpoint validates its input and is admin-only.
#[tokio::test]
async fn test_change_password_validates_input() {
    let app = TestApp::new().await;
    let token = app.bootstrap_admin().await;

    for bad in ["short", "        "] {
        let res = app
            .make_request_with_token(
                Some(&token),
                "POST",
                "/api/profile/password",
                Some(json!({ "new_password": bad })),
            )
            .await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{:?} should be rejected", bad);
    }

    // A non-admin caller is refused outright: only the admin bootstrap gate
    // uses this endpoint, everyone else resets passwords by email code.
    let (_, other_token) = app.provision_user().await;
    let res = app
        .make_request_with_token(
            Some(&other_token),
            "POST",
            "/api/profile/password",
            Some(json!({ "new_password": "a-valid-password" })),
        )
        .await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
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
    assert!(
        jcowork_gateway::auth::verify_password("someOtherPass", &promoted.password_hash)
            .unwrap_or(false),
        "promotion must not overwrite the account's existing password"
    );

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
    // A phone-provisioned account never sees the change-password screen.
    assert_eq!(body["must_change_password"], json!(false));
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
async fn test_change_password_closes_the_bootstrap_door() {
    let app = TestApp::new().await;

    // Deliberately stop short of changing the password: this test does it.
    let admin_token = app.bootstrap_admin().await;

    // The bootstrap password works until it is replaced.
    let (status, _) = app
        .login_raw(
            jcowork_gateway::auth::DEFAULT_ADMIN_USERNAME,
            jcowork_gateway::auth::DEFAULT_ADMIN_PASSWORD,
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    let new_password = app.change_admin_password(&admin_token).await;

    // The default admin password is a published constant, so replacing it has
    // to retire it — otherwise anyone who can reach the port keeps a way in.
    let (status, _) = app
        .login_raw(
            jcowork_gateway::auth::DEFAULT_ADMIN_USERNAME,
            jcowork_gateway::auth::DEFAULT_ADMIN_PASSWORD,
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "the bootstrap password must fail once replaced");

    let (status, body) = app
        .login_raw(jcowork_gateway::auth::DEFAULT_ADMIN_USERNAME, &new_password)
        .await;
    assert_eq!(status, StatusCode::OK, "{:?}", body);
    assert_eq!(body["must_change_password"], json!(false));

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

// ── Forced password change ──

/// An admin still on the bootstrap password may only reach the endpoints the
/// change-password screen needs; everything else is refused until the
/// password is replaced.
#[tokio::test]
async fn test_bootstrap_admin_is_locked_to_password_endpoints() {
    let app = TestApp::new().await;
    let token = app.bootstrap_admin().await;

    let res = app
        .make_request_with_token(Some(&token), "GET", "/api/profile", None)
        .await;
    assert_eq!(
        res.status(),
        StatusCode::OK,
        "the change-password screen must be able to read the profile"
    );

    for path in ["/api/providers", "/api/sessions", "/api/admin/users"] {
        let res = app
            .make_request_with_token(Some(&token), "GET", path, None)
            .await;
        assert_eq!(res.status(), StatusCode::FORBIDDEN, "{} must be gated", path);
        // The frontend branches on this code to redirect, so it has to be there.
        assert_eq!(json_body(res).await["code"], json!("password_change_required"));
    }

    // Changing the password lifts the gate for the very same token.
    app.change_admin_password(&token).await;
    let res = app
        .make_request_with_token(Some(&token), "GET", "/api/providers", None)
        .await;
    assert_eq!(
        res.status(),
        StatusCode::OK,
        "replacing the bootstrap password must unlock the rest of the API"
    );
}

// ── Admin mail-channel gate & configuration ──

/// A full, syntactically valid SMTP configuration body.
fn smtp_config_body() -> serde_json::Value {
    json!({
        "provider": "smtp",
        "smtp_host": "smtp.example.com",
        "smtp_port": 465,
        "smtp_username": "bot@example.com",
        "smtp_password": "mailbox-auth-code",
        "smtp_from": "bot@example.com",
    })
}

/// On a deployment whose mail channel still resolves `auto` to the dev
/// fallback, the admin's token is locked to the profile and mail config
/// endpoints; a stored SMTP configuration lifts the lock in place.
#[tokio::test]
async fn test_mail_auto_dev_locks_admin_until_configured() {
    let app = TestApp::with_mail_auto_dev().await;
    let token = app.seed_and_login_admin().await;

    // The gate allowlist: the profile (flag read) and the config endpoints.
    let res = app
        .make_request_with_token(Some(&token), "GET", "/api/profile", None)
        .await;
    assert_eq!(res.status(), StatusCode::OK, "profile must stay reachable");
    assert_eq!(
        json_body(res).await["must_configure_mail"],
        json!(true),
        "the profile must flag the pending mail setup"
    );
    let res = app
        .make_request_with_token(Some(&token), "GET", "/api/admin/mail", None)
        .await;
    assert_eq!(res.status(), StatusCode::OK, "mail config read must stay reachable");

    // Everything else answers with the mail gate.
    for path in ["/api/providers", "/api/sessions", "/api/skills", "/api/admin/users"] {
        let res = app
            .make_request_with_token(Some(&token), "GET", path, None)
            .await;
        assert_eq!(res.status(), StatusCode::FORBIDDEN, "{} must be gated", path);
        assert_eq!(json_body(res).await["code"], json!("mail_config_required"));
    }

    // Ordinary users are never locked behind the mail gate: they cannot
    // rescue themselves from the setup screen.
    let (_, user_token) = app.provision_user().await;
    let res = app
        .make_request_with_token(Some(&user_token), "GET", "/api/providers", None)
        .await;
    assert_eq!(
        res.status(),
        StatusCode::OK,
        "ordinary users must keep working while the admin's mail is unset"
    );

    // Storing a working SMTP configuration lifts the gate for the same token
    // — the runtime is rebuilt in place, no restart needed.
    let res = app
        .make_request_with_token(Some(&token), "PUT", "/api/admin/mail", Some(smtp_config_body()))
        .await;
    assert_eq!(res.status(), StatusCode::OK, "storing the SMTP config should succeed");
    let res = app
        .make_request_with_token(Some(&token), "GET", "/api/providers", None)
        .await;
    assert_eq!(res.status(), StatusCode::OK, "a configured mail channel must unlock the API");
    let res = app
        .make_request_with_token(Some(&token), "GET", "/api/profile", None)
        .await;
    assert_eq!(
        json_body(res).await["must_configure_mail"],
        json!(false),
        "the profile must stop flagging the mail setup"
    );
}

/// Declaring `provider = "dev"` is the admin's explicit choice: it lifts the
/// gate exactly like a full SMTP configuration would, and reads back as a
/// deliberate dev channel rather than an unconfigured one.
#[tokio::test]
async fn test_mail_gate_escape_hatch_declared_dev() {
    let app = TestApp::with_mail_auto_dev().await;
    let token = app.seed_and_login_admin().await;

    let res = app
        .make_request_with_token(Some(&token), "PUT", "/api/admin/mail", Some(json!({"provider": "dev"})))
        .await;
    assert_eq!(res.status(), StatusCode::OK, "declaring dev should succeed");
    let res = app
        .make_request_with_token(Some(&token), "GET", "/api/providers", None)
        .await;
    assert_eq!(res.status(), StatusCode::OK, "a declared dev channel must unlock the API");

    let res = app
        .make_request_with_token(Some(&token), "GET", "/api/admin/mail", None)
        .await;
    let body = json_body(res).await;
    assert_eq!(body["resolved"], json!("dev"));
    assert_eq!(body["auto_dev"], json!(false), "declared dev is not 'unconfigured'");
    assert_eq!(body["source"], json!("stored"), "the declaration itself is persisted");
}

/// The two bootstrap gates compose in order on a fresh deployment: bootstrap
/// password first, mail setup second, application after that.
#[tokio::test]
async fn test_sequential_admin_bootstrap_gates() {
    let app = TestApp::with_mail_auto_dev().await;
    let token = app.bootstrap_admin().await;

    // Both gates would apply, but the password one speaks first.
    let res = app
        .make_request_with_token(Some(&token), "GET", "/api/providers", None)
        .await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    assert_eq!(json_body(res).await["code"], json!("password_change_required"));

    app.change_admin_password(&token).await;
    let res = app
        .make_request_with_token(Some(&token), "GET", "/api/providers", None)
        .await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN, "the mail gate takes over");
    assert_eq!(json_body(res).await["code"], json!("mail_config_required"));

    let res = app
        .make_request_with_token(Some(&token), "PUT", "/api/admin/mail", Some(smtp_config_body()))
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    let res = app
        .make_request_with_token(Some(&token), "GET", "/api/providers", None)
        .await;
    assert_eq!(res.status(), StatusCode::OK, "both gates cleared — full access");
}

/// The mail config endpoints never echo the SMTP password; `GET` reports only
/// whether one is set, and every response is redacted the same way.
#[tokio::test]
async fn test_mail_config_get_redacts_password() {
    let app = TestApp::with_mail_auto_dev().await;
    let token = app.seed_and_login_admin().await;

    let res = app
        .make_request_with_token(Some(&token), "PUT", "/api/admin/mail", Some(smtp_config_body()))
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    let put_body = json_body(res).await;
    assert!(put_body.get("smtp_password").is_none(), "PUT must not echo the password");
    assert_eq!(put_body["password_set"], json!(true));

    let res = app
        .make_request_with_token(Some(&token), "GET", "/api/admin/mail", None)
        .await;
    let body = json_body(res).await;
    assert!(body.get("smtp_password").is_none(), "GET must not echo the password");
    assert_eq!(body["password_set"], json!(true));
    assert_eq!(body["smtp_host"], json!("smtp.example.com"));
    assert_eq!(body["resolved"], json!("smtp"));
    assert_eq!(body["source"], json!("stored"));
}

/// `PUT /api/admin/mail` is a merge: partial updates keep unspecified fields,
/// an empty password keeps the stored one, and `provider = "smtp"` with
/// incomplete credentials is refused rather than silently downgraded.
#[tokio::test]
async fn test_mail_config_put_validation_and_merge() {
    let app = TestApp::with_mail_auto_dev().await;
    let token = app.seed_and_login_admin().await;

    // SMTP without credentials must be refused up front.
    let res = app
        .make_request_with_token(Some(&token), "PUT", "/api/admin/mail", Some(json!({"provider": "smtp"})))
        .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST, "smtp with no credentials must be refused");

    // A port outside u16 fails JSON deserialization before the handler runs;
    // axum's Json rejection is 422 rather than a handler-level 400.
    let res = app
        .make_request_with_token(Some(&token), "PUT", "/api/admin/mail", Some(json!({"smtp_port": 99999})))
        .await;
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY, "an invalid port must be refused");

    // Full config, then a partial update that leaves the password out.
    let res = app
        .make_request_with_token(Some(&token), "PUT", "/api/admin/mail", Some(smtp_config_body()))
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    let res = app
        .make_request_with_token(
            Some(&token),
            "PUT",
            "/api/admin/mail",
            Some(json!({"smtp_host": "smtp2.example.com", "smtp_password": "", "allow_register": false})),
        )
        .await;
    assert_eq!(res.status(), StatusCode::OK, "a partial update must succeed");

    let res = app
        .make_request_with_token(Some(&token), "GET", "/api/admin/mail", None)
        .await;
    let body = json_body(res).await;
    assert_eq!(body["smtp_host"], json!("smtp2.example.com"), "the host must be updated");
    assert_eq!(body["password_set"], json!(true), "an empty password must keep the stored one");
    assert_eq!(body["allow_register"], json!(false), "the register toggle must persist");
}

/// The test endpoint goes through the same channel as real codes — capture
/// sees the recipient — but its `test:` probe key can never collide with a
/// real verification code, and the shared cooldown throttles repeat probes.
#[tokio::test]
async fn test_mail_test_endpoint_sends_and_throttles() {
    let app = TestApp::new().await;
    let token = app.seed_and_login_admin().await;

    // Admin-only.
    let (_, user_token) = app.provision_user().await;
    let res = app
        .make_request_with_token(Some(&user_token), "POST", "/api/admin/mail/test", Some(json!({"to": "a@example.com"})))
        .await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN, "non-admins may not probe the channel");

    let res = app
        .make_request_with_token(Some(&token), "POST", "/api/admin/mail/test", Some(json!({"to": "friend@example.com"})))
        .await;
    assert_eq!(res.status(), StatusCode::OK, "the first probe should go out");
    let sent = app.mail_sent.lock().expect("capture lock poisoned").clone();
    assert_eq!(sent.len(), 1, "exactly one test message");
    assert_eq!(sent[0].0, "friend@example.com", "the recipient is the test address");
    assert!(!sent[0].1.starts_with("test:"), "the probe key must not leak into the message key");

    let res = app
        .make_request_with_token(Some(&token), "POST", "/api/admin/mail/test", Some(json!({"to": "friend@example.com"})))
        .await;
    assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS, "the shared cooldown throttles probes");
}

/// An existing deployment whose admin already changed the bootstrap password
/// gets the mail-setup flag on the next password login — the upgrade path for
/// `is_auto_dev` deployments.
#[tokio::test]
async fn test_admin_login_flags_mail_setup_on_upgrade() {
    let app = TestApp::with_mail_auto_dev().await;
    let token = app.seed_and_login_admin().await;
    let new_password = "replaced-admin-pass";

    let (status, resp) = app
        .login_raw(jcowork_gateway::auth::DEFAULT_ADMIN_USERNAME, new_password)
        .await;
    assert_eq!(status, StatusCode::OK, "the replaced password logs in");
    assert_eq!(resp["must_change_password"], json!(false));
    assert_eq!(resp["must_configure_mail"], json!(true), "the mail setup must be flagged");

    // Once the channel is configured the flag goes away on the next login.
    let res = app
        .make_request_with_token(Some(&token), "PUT", "/api/admin/mail", Some(json!({"provider": "dev"})))
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    let (status, resp) = app
        .login_raw(jcowork_gateway::auth::DEFAULT_ADMIN_USERNAME, new_password)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(resp["must_configure_mail"], json!(false));
}

/// `DELETE /api/admin/mail` drops the stored row and falls back to the
/// environment — on a machine with no `JCWORK_MAIL_*` that is the auto→dev
/// fallback again, so the admin gate re-arms.
#[tokio::test]
async fn test_delete_stored_mail_config_rearms_gate() {
    let app = TestApp::with_mail_auto_dev().await;
    let token = app.seed_and_login_admin().await;

    let res = app
        .make_request_with_token(Some(&token), "PUT", "/api/admin/mail", Some(smtp_config_body()))
        .await;
    assert_eq!(res.status(), StatusCode::OK);

    let res = app
        .make_request_with_token(Some(&token), "DELETE", "/api/admin/mail", None)
        .await;
    assert_eq!(res.status(), StatusCode::OK, "clearing the stored config should succeed");
    let body = json_body(res).await;
    assert_eq!(body["source"], json!("env"), "env applies again after the delete");

    let res = app
        .make_request_with_token(Some(&token), "GET", "/api/providers", None)
        .await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN, "the gate re-arms on the fallback");
    assert_eq!(json_body(res).await["code"], json!("mail_config_required"));
}

// ── Admin phone assignment ──

/// Assigning a number is the only way back into an account that predates SMS
/// login, so it has to work end to end. The stored password is kept — only
/// the password door's admin-only rule stops it from logging in.
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

    // Assigning keeps the stored hash: binding a phone adds SMS login but
    // never destroys the account's own credential.
    let stored = app.user_store.get_user_by_id(&legacy.id).await.unwrap().unwrap();
    assert!(
        jcowork_gateway::auth::verify_password("legacypass1", &stored.password_hash)
            .unwrap_or(false),
        "assigning a phone must keep the account's password hash"
    );
    let (status, _) = app.login_raw("legacy", "legacypass1").await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "non-admin password login stays closed regardless of the stored hash"
    );

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

    // The admin cannot unbind itself: the number is the admin's SMS-login
    // channel, and the guard keeps it from being cut off by a stray click
    // (the admin password no longer depends on the phone either way).
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

/// Register (image captcha, no mailed code) → token works despite having no
/// phone (the gate keys on phone-or-email) → the credentials mail carries the
/// chosen account name and the plaintext password → login by account name and
/// by registration email → duplicate address or duplicate name is a conflict.
#[tokio::test]
async fn test_email_register_login_and_gate_passes_without_phone() {
    let app = TestApp::new().await;
    let email = app.next_email();
    let username = "alice";
    let password = "initial-pass-1";

    // Cheap rejections happen before the captcha is spent, so one captcha can
    // carry both the bad attempt and the good one.
    let (captcha_id, captcha_code) = app.fetch_captcha().await;
    let (status, body) = app
        .post_public(
            "/api/auth/email/register",
            json!({
                "username": "bad name with spaces",
                "email": email,
                "password": password,
                "captcha_id": captcha_id,
                "captcha_code": captcha_code,
            }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{:?}", body);
    assert_eq!(
        body["error"],
        json!("Username cannot contain spaces, @, / or control characters")
    );

    let (status, body) = app
        .post_public(
            "/api/auth/email/register",
            json!({
                "username": username,
                "email": email,
                "password": password,
                "captcha_id": captcha_id,
                "captcha_code": captcha_code,
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{:?}", body);
    assert_eq!(body["is_admin"], json!(false));
    assert_eq!(body["email_sent"], json!(true));
    assert_eq!(
        body["must_change_password"],
        json!(false),
        "an email-born account never sees the change-password screen"
    );
    let user_id = body["user_id"].as_str().unwrap().to_string();
    assert_eq!(body["username"], json!(username), "the name is the caller's choice");

    // The credentials mail went to the right address and carries the account
    // name and the password the user chose.
    {
        let messages = app.mail_messages.lock().expect("capture lock poisoned");
        let matches: Vec<&(String, String, String)> =
            messages.iter().filter(|(e, _, _)| e == &email).collect();
        assert_eq!(matches.len(), 1, "exactly one credentials mail: {:?}", messages);
        let (_, subject, mail_body) = matches[0];
        assert_eq!(subject, "Jcowork 注册成功");
        assert!(mail_body.contains(username), "the mail names the account: {mail_body}");
        assert!(mail_body.contains(password), "the mail carries the password: {mail_body}");
    }
    // Registration is captcha-gated, not code-gated: nothing was mailed as a
    // verification code.
    assert!(
        app.mail_sent.lock().expect("capture lock poisoned").is_empty(),
        "registration must not mint an emailed code"
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

    // Login works with the account name...
    let (status, body) = app
        .post_public(
            "/api/auth/email/login",
            json!({ "username": username, "password": password }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{:?}", body);
    assert_eq!(body["user_id"], json!(user_id));
    assert_eq!(body["must_change_password"], json!(false));

    // ...and the registration email still works for accounts whose mailed
    // credentials only ever named the address. Case-insensitively, because
    // the server normalizes before the email lookup.
    let (status, body) = app
        .post_public(
            "/api/auth/email/login",
            json!({ "username": email.to_uppercase(), "password": password }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{:?}", body);
    assert_eq!(body["user_id"], json!(user_id));

    let (status, body) = app
        .post_public(
            "/api/auth/email/login",
            json!({ "username": username, "password": "wrong-password" }),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"], json!("Invalid username or password"));

    // Unknown names fail exactly like wrong passwords.
    let (status, body) = app
        .post_public(
            "/api/auth/email/login",
            json!({ "username": "nobody-here", "password": password }),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"], json!("Invalid username or password"));

    // Re-registering the address is a conflict — behind a fresh captcha, since
    // every attempt burns one.
    let (status, body) = app.register_email("alice2", &email, "another-pass-2").await;
    assert_eq!(status, StatusCode::CONFLICT, "{:?}", body);
    assert_eq!(body["error"], json!("Email address is already registered"));
    // ...and so is re-registering the account name.
    let (status, body) = app.register_email(username, &app.next_email(), "another-pass-2").await;
    assert_eq!(status, StatusCode::CONFLICT, "{:?}", body);
    assert_eq!(body["error"], json!("Username already exists"));
    // The duplicates never mailed anything.
    let messages = app.mail_messages.lock().expect("capture lock poisoned");
    assert_eq!(
        messages.iter().filter(|(e, _, _)| e == &email).count(),
        1,
        "only the successful registration mailed credentials"
    );
}

/// The registration captcha: missing fields read as a readable 400 (not
/// axum's 422), a wrong answer is refused, every attempt burns the captcha —
/// even a correct code cannot revive a spent one — and a fresh captcha carries
/// the registration through. Registration never touches the mailed-code
/// channel, whose attempt counting belongs to password reset alone.
#[tokio::test]
async fn test_register_captcha_wrong_burns_and_single_use() {
    let app = TestApp::new().await;
    let email = app.next_email();

    // Missing captcha fields: readable 400 rather than the extractor's 422,
    // so a legacy client sees prose it can localize.
    let (status, body) = app
        .post_public(
            "/api/auth/email/register",
            json!({ "username": "capuser", "email": email, "password": "some-pass-123" }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{:?}", body);
    assert_eq!(body["error"], json!("Captcha is required"));

    let (captcha_id, captcha_code) = app.fetch_captcha().await;

    // Wrong answer: refused...
    let (status, body) = app
        .post_public(
            "/api/auth/email/register",
            json!({
                "username": "capuser",
                "email": email,
                "password": "some-pass-123",
                "captcha_id": captcha_id,
                "captcha_code": "NOPE0",
            }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{:?}", body);
    assert_eq!(body["error"], json!("Captcha is incorrect or has expired"));

    // ...and burned: the attempt itself spent it, so even the right code
    // cannot pass a second time.
    let (status, body) = app
        .post_public(
            "/api/auth/email/register",
            json!({
                "username": "capuser",
                "email": email,
                "password": "some-pass-123",
                "captcha_id": captcha_id,
                "captcha_code": captcha_code,
            }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{:?}", body);
    assert_eq!(body["error"], json!("Captcha is incorrect or has expired"));

    // An unknown id fails the same way — nothing is revealed about which ids
    // ever existed.
    let (status, body) = app
        .post_public(
            "/api/auth/email/register",
            json!({
                "username": "capuser",
                "email": email,
                "password": "some-pass-123",
                "captcha_id": "cap-never-issued",
                "captcha_code": "WHATEVER",
            }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{:?}", body);
    assert_eq!(body["error"], json!("Captcha is incorrect or has expired"));

    // A fresh captcha carries the same registration through.
    let (status, body) = app.register_email("capuser", &email, "some-pass-123").await;
    assert_eq!(status, StatusCode::OK, "{:?}", body);

    // All of the above cost no mailed code: that channel is reset-only now.
    assert!(
        app.mail_sent.lock().expect("capture lock poisoned").is_empty(),
        "captcha rejections must not mint emailed codes"
    );
}

/// The credentials mail is real mail, so it is throttled like one: a second
/// registration for the same address inside the 60-second cooldown is a 429.
/// The duplicate check has to pass first, which in practice means the first
/// account was removed in the meantime.
#[tokio::test]
async fn test_register_credentials_mail_shares_the_cooldown() {
    let app = TestApp::new().await;
    let email = app.next_email();

    let (status, body) = app.register_email("throttle-user", &email, "throttle-pass-1").await;
    assert_eq!(status, StatusCode::OK, "{:?}", body);
    let user_id = body["user_id"].as_str().unwrap().to_string();

    // Remove the account so the duplicate check passes again; the regcred
    // cooldown is keyed on the address, not on the account.
    app.user_store.permanently_delete_user(&user_id).await.unwrap();

    let (status, body) = app
        .register_email("throttle-user", &email, "throttle-pass-2")
        .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{:?}", body);
    assert_eq!(body["error"], json!("Please wait before requesting another code"));
    assert!(
        body["retry_after"].as_u64().unwrap_or(0) > 0,
        "the 429 carries a retry hint: {:?}",
        body
    );
    // The refused attempt never reached the mail channel.
    let messages = app.mail_messages.lock().expect("capture lock poisoned");
    assert_eq!(
        messages.iter().filter(|(e, _, _)| e == &email).count(),
        1,
        "only the first registration mailed credentials"
    );
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
    app.user_store
        .create_email_user("reset-user", &email, &old_hash)
        .await
        .unwrap();

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
            json!({ "username": email, "password": "fresh-pass-45" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "the new password must authenticate: {:?}", body);

    let (status, body) = app
        .post_public(
            "/api/auth/email/login",
            json!({ "username": email, "password": "old-pass-123" }),
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
/// channel: codes still go out, but registration is refused outright — before
/// the captcha is even judged.
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
            json!({
                "username": "shut-out",
                "email": email,
                "password": "some-pass-89",
                "captcha_id": "cap-anything",
                "captcha_code": "ANYPE",
            }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{:?}", body);
    assert_eq!(body["error"], json!("Email registration is disabled"));
    // Refused before the mail channel was touched for credentials.
    assert!(
        app.mail_messages.lock().expect("capture lock poisoned").is_empty(),
        "a refused registration mails nothing"
    );

    // The code that did go out still serves the reset flow.
    let (status, body) = app
        .post_public(
            "/api/auth/password/reset",
            json!({ "email": email, "code": code, "new_password": "fresh-pass-89" }),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{:?}", body);
    assert_eq!(body["error"], json!("Account not found"));
}

// ── Aggregated AI feed ──

#[tokio::test]
async fn test_feed_requires_auth() {
    let app = TestApp::new().await;
    let res = app
        .make_request_with_token(None, "GET", "/api/feed", None)
        .await;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    let res = app
        .make_request_with_token(None, "GET", "/api/feed/results/whatever", None)
        .await;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_own_feed_public_feed_and_result_visibility() {
    let app = TestApp::new().await;
    let (a_id, a_token) = app.provision_user().await;
    let (_b_id, b_token) = app.provision_user().await;
    let admin_token = app.seed_and_login_admin().await;

    // User A creates a periodic task and stores one execution result.
    let res = app
        .make_request_with_token(
            Some(&a_token),
            "POST",
            "/api/cron-jobs",
            Some(json!({
                "name": "Daily news digest",
                "prompt": "Summarize today's AI news",
                "model": "mock:test-model",
                "frequency": "daily",
                "hour": 9,
                "minute": 0,
                "second": 0,
            })),
        )
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    let body: serde_json::Value =
        serde_json::from_slice(&axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap())
            .unwrap();
    let job_id = body["id"].as_str().unwrap().to_string();

    let res = app
        .make_request_with_token(
            Some(&a_token),
            "POST",
            &format!("/api/cron-jobs/{}/results", job_id),
            Some(json!({
                "output": "# Headline one\n\nLong output body with **markdown**.",
                "status": "success",
            })),
        )
        .await;
    assert_eq!(res.status(), StatusCode::OK);

    // A's own feed lists the result as a task item with an excerpt and job metadata.
    let res = app
        .make_request_with_token(Some(&a_token), "GET", "/api/feed?offset=0&limit=20", None)
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    let feed: serde_json::Value =
        serde_json::from_slice(&axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap())
            .unwrap();
    let items = feed["items"].as_array().unwrap().clone();
    assert_eq!(items.len(), 1, "feed: {:?}", feed);
    assert_eq!(items[0]["type"], json!("task"));
    assert_eq!(items[0]["job_name"], json!("Daily news digest"));
    assert_eq!(items[0]["status"], json!("success"));
    assert!(items[0]["excerpt"].as_str().unwrap().contains("Headline one"));
    assert_eq!(feed["has_more"], json!(false));
    let result_id = items[0]["id"].as_str().unwrap().to_string();

    // The owner can fetch the full output by result id.
    let res = app
        .make_request_with_token(Some(&a_token), "GET", &format!("/api/feed/results/{}", result_id), None)
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    let full: serde_json::Value =
        serde_json::from_slice(&axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap())
            .unwrap();
    assert!(full["output"].as_str().unwrap().contains("**markdown**"));
    assert_eq!(full["job_name"], json!("Daily news digest"));

    // While A is private, another account sees neither the result nor A's feed.
    let res = app
        .make_request_with_token(Some(&b_token), "GET", &format!("/api/feed/results/{}", result_id), None)
        .await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let res = app
        .make_request_with_token(Some(&b_token), "GET", &format!("/api/public-users/{}/feed", a_id), None)
        .await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    // Admin flips A public: B now reads A's task through the public feed
    // and can fetch the full result by id.
    let res = app
        .make_request_with_token(
            Some(&admin_token),
            "POST",
            &format!("/api/admin/users/{}/public", a_id),
            Some(json!({ "is_public": true })),
        )
        .await;
    assert_eq!(res.status(), StatusCode::OK);

    let res = app
        .make_request_with_token(Some(&b_token), "GET", &format!("/api/public-users/{}/feed?limit=5", a_id), None)
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    let feed: serde_json::Value =
        serde_json::from_slice(&axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap())
            .unwrap();
    let items = feed["items"].as_array().unwrap().clone();
    assert_eq!(items.len(), 1, "public feed: {:?}", feed);
    assert_eq!(items[0]["type"], json!("task"));
    assert_eq!(items[0]["job_name"], json!("Daily news digest"));

    let res = app
        .make_request_with_token(Some(&b_token), "GET", &format!("/api/feed/results/{}", result_id), None)
        .await;
    assert_eq!(res.status(), StatusCode::OK);

    // Unknown result ids are a plain 404.
    let res = app
        .make_request_with_token(Some(&a_token), "GET", "/api/feed/results/no-such-result", None)
        .await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

// ── AI teaser summaries on feed items ──

/// The fixed string the mock LLM returns for every non-streaming call; a
/// stored summary must equal exactly this.
const MOCK_SUMMARY: &str = "This is a mock response for testing.";

async fn get_feed_json(app: &TestApp, token: &str) -> serde_json::Value {
    let res = app
        .make_request_with_token(Some(token), "GET", "/api/feed?limit=50", None)
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    serde_json::from_slice(&axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap())
        .unwrap()
}

/// Create one cron job owned by `token`'s account and return its id.
async fn create_cron_job(app: &TestApp, token: &str) -> String {
    let res = app
        .make_request_with_token(
            Some(token),
            "POST",
            "/api/cron-jobs",
            Some(json!({
                "name": "Daily news digest",
                "prompt": "Summarize today's AI news",
                "model": "mock:test-model",
                "frequency": "daily",
                "hour": 9,
                "minute": 0,
                "second": 0,
            })),
        )
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    let body: serde_json::Value =
        serde_json::from_slice(&axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap())
            .unwrap();
    body["id"].as_str().unwrap().to_string()
}

async fn store_result(app: &TestApp, token: &str, job_id: &str, output: &str, status: &str) {
    let res = app
        .make_request_with_token(
            Some(token),
            "POST",
            &format!("/api/cron-jobs/{}/results", job_id),
            Some(json!({ "output": output, "status": status })),
        )
        .await;
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_feed_task_summary_generated_via_write_hook() {
    let app = TestApp::new().await;
    let (_a_id, a_token) = app.provision_user().await;
    let job_id = create_cron_job(&app, &a_token).await;

    // A long successful output earns an AI teaser; generation is a spawned
    // background task, so poll the feed until it lands.
    let long_output = format!("# AI Daily\n\n{}", "A concrete insight paragraph. ".repeat(20));
    store_result(&app, &a_token, &job_id, &long_output, "success").await;

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut summary = String::new();
    let mut excerpt = String::new();
    loop {
        let feed = get_feed_json(&app, &a_token).await;
        let items = feed["items"].as_array().unwrap();
        assert_eq!(items.len(), 1, "feed: {:?}", feed);
        assert_eq!(items[0]["type"], json!("task"));
        excerpt = items[0]["excerpt"].as_str().unwrap().to_string();
        if let Some(s) = items[0]["summary"].as_str() {
            if !s.is_empty() {
                summary = s.to_string();
                break;
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "summary never generated, feed: {:?}",
            feed
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(summary, MOCK_SUMMARY);
    // The raw excerpt stays a truncation of the original output.
    assert!(excerpt.contains("AI Daily"));

    // Guards: a short output and a failed run never get a summary — the card
    // keeps showing the raw text, which already tells the story.
    store_result(&app, &a_token, &job_id, "short output", "success").await;
    store_result(
        &app,
        &a_token,
        &job_id,
        &format!("Execution failed: {}", "boom ".repeat(100)),
        "error",
    ).await;
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let feed = get_feed_json(&app, &a_token).await;
    let items = feed["items"].as_array().unwrap();
    assert_eq!(items.len(), 3, "feed: {:?}", feed);
    for item in items {
        let status = item["status"].as_str().unwrap();
        let summary = item["summary"].as_str().unwrap();
        if status == "error" || item["excerpt"].as_str().unwrap() == "short output" {
            assert_eq!(summary, "", "guard case must stay unsummarized: {:?}", item);
        }
    }
}

#[tokio::test]
async fn test_feed_doc_summary_backfilled_on_read() {
    let app = TestApp::new().await;
    let (a_id, a_token) = app.provision_user().await;

    // Write files straight into A's workspace and index them through the
    // storage layer — no HTTP write hook fires, so the summary can only come
    // from the feed's lazy backfill on first read.
    let ws = format!("{}/{}/workspace", app.data_dir, a_id);
    std::fs::create_dir_all(&ws).unwrap();
    let long_body = format!("# Quarterly report\n\n{}", "Revenue grew in every segment. ".repeat(20));
    std::fs::write(format!("{}/report.md", ws), &long_body).unwrap();
    std::fs::write(format!("{}/tiny.md", ws), "# Tiny\n\nshort note").unwrap();
    let index = jcowork_storage::WorkspaceIndex::cached(&app.data_dir, &a_id)
        .await
        .unwrap();
    index.add_document("report.md", &ws).await.unwrap();
    index.add_document("tiny.md", &ws).await.unwrap();

    // First read kicks off generation; poll until the long document's teaser
    // lands. The tiny document must never get one.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut report_summary = String::new();
    loop {
        let feed = get_feed_json(&app, &a_token).await;
        let items = feed["items"].as_array().unwrap();
        assert_eq!(items.len(), 2, "feed: {:?}", feed);
        let mut seen = std::collections::HashMap::new();
        for item in items {
            assert_eq!(item["type"], json!("document"));
            seen.insert(
                item["filename"].as_str().unwrap().to_string(),
                item["summary"].as_str().unwrap().to_string(),
            );
        }
        assert_eq!(
            seen.get("tiny.md").map(String::as_str),
            Some(""),
            "short documents must stay unsummarized"
        );
        if let Some(s) = seen.get("report.md") {
            if !s.is_empty() {
                report_summary = s.clone();
                break;
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "doc summary never generated, feed: {:?}",
            feed
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(report_summary, MOCK_SUMMARY);

    // The snippet field is untouched: still the raw content head, so the
    // client fallback keeps working for anything not yet summarized.
    let feed = get_feed_json(&app, &a_token).await;
    for item in feed["items"].as_array().unwrap() {
        if item["filename"] == json!("report.md") {
            assert!(item["snippet"].as_str().unwrap().contains("Quarterly report"));
        }
    }
}

#[tokio::test]
async fn test_public_document_content_requires_no_login() {
    let app = TestApp::new().await;
    let (a_id, a_token) = app.provision_user().await;

    let ws = format!("{}/{}/workspace", app.data_dir, a_id);
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::write(format!("{}/share.md", ws), "# Shared\n\npublic body text").unwrap();
    let index = jcowork_storage::WorkspaceIndex::cached(&app.data_dir, &a_id)
        .await
        .unwrap();
    index.add_document("share.md", &ws).await.unwrap();

    // The document's own feed entry carries the public flag (off by default).
    let feed = get_feed_json(&app, &a_token).await;
    let items = feed["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["is_public"], json!(false));

    // Mark it public, then flip the account public (admin) — the state a
    // shareable standalone-doc link assumes.
    let res = app
        .make_request_with_token(
            Some(&a_token),
            "PUT",
            "/api/workspace/index/public",
            Some(json!({ "path": "share.md", "is_public": true })),
        )
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    let admin_token = app.seed_and_login_admin().await;
    let res = app
        .make_request_with_token(
            Some(&admin_token),
            "POST",
            &format!("/api/admin/users/{}/public", a_id),
            Some(json!({ "is_public": true })),
        )
        .await;
    assert_eq!(res.status(), StatusCode::OK);

    // The share link's backend: the content endpoint answers without any
    // Authorization header at all.
    let res = app
        .make_request_with_token(
            None,
            "GET",
            &format!(
                "/api/public-users/{}/documents/content?path=share.md&offset=0&limit=100",
                a_id
            ),
            None,
        )
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    let body: serde_json::Value =
        serde_json::from_slice(&axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap())
            .unwrap();
    assert!(body["content"].as_str().unwrap().contains("public body text"));

    // The feed entry now reports is_public, so the client builds a login-free
    // share URL for it.
    let feed = get_feed_json(&app, &a_token).await;
    assert_eq!(feed["items"][0]["is_public"], json!(true));
}

/// The raw-file download endpoint behind the DocViewer's "Original PDF" mode:
/// public document + public account serves the file without login; a document
/// that is not flagged public stays hidden.
#[tokio::test]
async fn test_public_document_download_serves_raw_file() {
    let app = TestApp::new().await;
    let (a_id, a_token) = app.provision_user().await;

    let ws = format!("{}/{}/workspace", app.data_dir, a_id);
    std::fs::create_dir_all(&ws).unwrap();
    // Real PDF streams contain binary (non-UTF-8) bytes — the download
    // endpoint must serve them byte-for-byte, not decode them as text.
    let pdf_bytes = b"%PDF-1.4\nraw pdf bytes: \xff\xfe\x00\x89PNG-like binary\n%%EOF";
    std::fs::write(format!("{}/doc.pdf", ws), pdf_bytes).unwrap();
    let index = jcowork_storage::WorkspaceIndex::cached(&app.data_dir, &a_id)
        .await
        .unwrap();
    index.add_document("doc.pdf", &ws).await.unwrap();

    // Not public yet: the anonymous download endpoint must refuse.
    let res = app
        .make_request_with_token(
            None,
            "GET",
            &format!("/api/public-users/{}/documents/download?path=doc.pdf", a_id),
            None,
        )
        .await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    // Mark the document public and the account public (admin).
    let res = app
        .make_request_with_token(
            Some(&a_token),
            "PUT",
            "/api/workspace/index/public",
            Some(json!({ "path": "doc.pdf", "is_public": true })),
        )
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    let admin_token = app.seed_and_login_admin().await;
    let res = app
        .make_request_with_token(
            Some(&admin_token),
            "POST",
            &format!("/api/admin/users/{}/public", a_id),
            Some(json!({ "is_public": true })),
        )
        .await;
    assert_eq!(res.status(), StatusCode::OK);

    // Now the original file is served without any Authorization header,
    // with the PDF content type so the viewer renders it inline.
    let res = app
        .make_request_with_token(
            None,
            "GET",
            &format!("/api/public-users/{}/documents/download?path=doc.pdf", a_id),
            None,
        )
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/pdf")
    );
    let body = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
    assert_eq!(&body[..], pdf_bytes);
}

/// PUT /api/workspace/index/content overwrites the indexed (parsed) preview
/// text of a document — the storage behind the PDF parsed-content editor.
/// The raw file on disk stays untouched, and unindexed paths are refused.
#[tokio::test]
async fn test_update_indexed_content_overwrites_parsed_preview() {
    let app = TestApp::new().await;
    let (a_id, a_token) = app.provision_user().await;

    let ws = format!("{}/{}/workspace", app.data_dir, a_id);
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::write(format!("{}/note.md", ws), "# Original content\n").unwrap();
    let index = jcowork_storage::WorkspaceIndex::cached(&app.data_dir, &a_id)
        .await
        .unwrap();
    index.add_document("note.md", &ws).await.unwrap();

    // Unindexed path → 404.
    let res = app
        .make_request_with_token(
            Some(&a_token),
            "PUT",
            "/api/workspace/index/content",
            Some(json!({ "path": "missing.md", "content": "nope" })),
        )
        .await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    // Overwrite the indexed content (as the PDF parsed-content editor does).
    let res = app
        .make_request_with_token(
            Some(&a_token),
            "PUT",
            "/api/workspace/index/content",
            Some(json!({ "path": "note.md", "content": "# Edited by the user\n" })),
        )
        .await;
    assert_eq!(res.status(), StatusCode::OK);

    // The content endpoint now serves the edited text.
    let res = app
        .make_request_with_token(
            Some(&a_token),
            "GET",
            "/api/workspace/index/content?path=note.md",
            None,
        )
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["content"], "# Edited by the user\n");

    // The raw file on disk is untouched.
    let raw = std::fs::read_to_string(format!("{}/note.md", ws)).unwrap();
    assert_eq!(raw, "# Original content\n");
}
