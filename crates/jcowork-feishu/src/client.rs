//! Feishu Open API client — send messages and manage tenant tokens.

use anyhow::Result;
use serde::Deserialize;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::debug;

/// Cached tenant access token.
#[derive(Debug, Clone)]
struct CachedToken {
    token: String,
    expires_at: chrono::DateTime<chrono::Utc>,
}

/// Feishu API client.
#[derive(Debug, Clone)]
pub struct FeishuClient {
    app_id: String,
    app_secret: String,
    /// Open-platform base URL. Feishu (`open.feishu.cn`) by default;
    /// overridable so a local mock can stand in during tests and so a Lark
    /// (international) deployment can point at its own domain.
    base_url: String,
    http: reqwest::Client,
    token_cache: Arc<RwLock<Option<CachedToken>>>,
}

/// Response from tenant_access_token API.
#[derive(Debug, Deserialize)]
struct TokenResponse {
    tenant_access_token: String,
    expire: i64,
}

/// Response from send message API.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct MessageResponse {
    code: i64,
    msg: Option<String>,
    data: Option<serde_json::Value>,
}

impl FeishuClient {
    pub fn new(app_id: String, app_secret: String) -> Self {
        Self::with_base_url(app_id, app_secret, default_base_url())
    }

    pub fn with_base_url(app_id: String, app_secret: String, base_url: String) -> Self {
        Self {
            app_id,
            app_secret,
            base_url,
            http: reqwest::Client::new(),
            token_cache: Arc::new(RwLock::new(None)),
        }
    }

    pub fn app_id(&self) -> &str {
        &self.app_id
    }

    /// Get a valid tenant access token, refreshing if expired.
    ///
    /// Public because the `lark_cli` tool hands the token to the official
    /// CLI: it only accepts externally-provided credentials through
    /// `LARKSUITE_CLI_TENANT_ACCESS_TOKEN`.
    pub async fn tenant_token(&self) -> Result<String> {
        self.get_tenant_token().await
    }

    /// Get a valid tenant access token, refreshing if expired.
    async fn get_tenant_token(&self) -> Result<String> {
        // Check cache first
        {
            let cache = self.token_cache.read().await;
            if let Some(cached) = cache.as_ref() {
                if cached.expires_at > chrono::Utc::now() {
                    return Ok(cached.token.clone());
                }
            }
        }

        // Refresh token
        debug!("Refreshing Feishu tenant access token");
        let resp = self
            .http
            .post(format!(
                "{}/open-apis/auth/v3/tenant_access_token/internal",
                self.base_url
            ))
            .json(&serde_json::json!({
                "app_id": self.app_id,
                "app_secret": self.app_secret
            }))
            .send()
            .await?;

        let token_resp: TokenResponse = resp.json().await?;
        if token_resp.tenant_access_token.is_empty() {
            anyhow::bail!("Feishu returned an empty tenant access token");
        }
        let new_token = CachedToken {
            token: token_resp.tenant_access_token.clone(),
            // Expire 5 minutes early to avoid edge cases
            expires_at: chrono::Utc::now() + chrono::Duration::seconds(token_resp.expire - 300),
        };

        let token = new_token.token.clone();
        {
            let mut cache = self.token_cache.write().await;
            *cache = Some(new_token);
        }

        Ok(token)
    }

    /// Reply to a specific message in a chat.
    pub async fn reply_message(&self, message_id: &str, text: &str) -> Result<()> {
        let token = self.get_tenant_token().await?;

        let url = format!(
            "{}/open-apis/im/v1/messages/{}/reply",
            self.base_url, message_id
        );

        let content = serde_json::json!({"text": text}).to_string();

        let resp = self
            .http
            .post(&url)
            .header("Authorization", format!("Bearer {}", token))
            .header("Content-Type", "application/json; charset=utf-8")
            .json(&serde_json::json!({
                "content": content,
                "msg_type": "text"
            }))
            .send()
            .await?;

        let status = resp.status();
        let body = resp.text().await?;
        debug!(%status, %body, "Feishu reply response");

        if !status.is_success() {
            tracing::warn!(%status, %body, "Feishu reply failed");
            anyhow::bail!("Feishu reply failed ({}): {}", status, body);
        }

        Ok(())
    }

    /// Send a message to a chat (not a reply, but a new message).
    #[allow(dead_code)]
    pub async fn send_message(&self, chat_id: &str, text: &str) -> Result<()> {
        let token = self.get_tenant_token().await?;

        let url = format!("{}/open-apis/im/v1/messages", self.base_url);

        let content = serde_json::json!({"text": text}).to_string();

        let resp = self
            .http
            .post(url)
            .header("Authorization", format!("Bearer {}", token))
            .header("Content-Type", "application/json; charset=utf-8")
            .json(&serde_json::json!({
                "chat_id": chat_id,
                "content": content,
                "msg_type": "text"
            }))
            .send()
            .await?;

        let status = resp.status();
        let body = resp.text().await?;
        debug!(%status, %body, "Feishu send response");

        if !status.is_success() {
            tracing::warn!(%status, %body, "Feishu send failed");
            anyhow::bail!("Feishu send failed ({}): {}", status, body);
        }

        Ok(())
    }
}

/// Open-platform base URL: `JCWORK_FEISHU_API_BASE` wins so tests can point
/// the client at a local mock; production uses Feishu's public endpoint.
fn default_base_url() -> String {
    std::env::var("JCWORK_FEISHU_API_BASE")
        .unwrap_or_else(|_| "https://open.feishu.cn".to_string())
        .trim_end_matches('/')
        .to_string()
}
