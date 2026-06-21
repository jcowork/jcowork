//! Feishu configuration from environment variables.

use serde::{Deserialize, Serialize};

/// Feishu bot configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeishuConfig {
    /// App ID from Feishu Developer Console.
    pub app_id: String,
    /// App Secret from Feishu Developer Console.
    pub app_secret: String,
    /// Verification Token for event URL validation.
    pub verification_token: String,
    /// Encrypt Key for event payload decryption (optional).
    pub encrypt_key: String,
}

impl FeishuConfig {
    /// Load config from environment variables.
    pub fn from_env() -> Option<Self> {
        let app_id = std::env::var("FEISHU_APP_ID").ok()?;
        let app_secret = std::env::var("FEISHU_APP_SECRET").ok()?;
        if app_id.is_empty() || app_secret.is_empty() {
            return None;
        }
        Some(Self {
            app_id,
            app_secret,
            verification_token: std::env::var("FEISHU_VERIFICATION_TOKEN").unwrap_or_default(),
            encrypt_key: std::env::var("FEISHU_ENCRYPT_KEY").unwrap_or_default(),
        })
    }

    /// Check if Feishu is configured.
    pub fn is_configured(&self) -> bool {
        !self.app_id.is_empty() && !self.app_secret.is_empty()
    }
}
