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
        Self {
            app_id,
            app_secret,
            http: reqwest::Client::new(),
            token_cache: Arc::new(RwLock::new(None)),
        }
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
            .post("https://open.feishu.cn/open-apis/auth/v3/tenant_access_token/internal")
            .json(&serde_json::json!({
                "app_id": self.app_id,
                "app_secret": self.app_secret
            }))
            .send()
            .await?;

        let token_resp: TokenResponse = resp.json().await?;
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
            "https://open.feishu.cn/open-apis/im/v1/messages/{}/reply",
            message_id
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
        }

        Ok(())
    }

    /// Send a message to a chat (not a reply, but a new message).
    #[allow(dead_code)]
    pub async fn send_message(&self, chat_id: &str, text: &str) -> Result<()> {
        let token = self.get_tenant_token().await?;

        let url = "https://open.feishu.cn/open-apis/im/v1/messages";

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
        }

        Ok(())
    }
}
