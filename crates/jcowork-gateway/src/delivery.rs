//! Platform delivery routing.

/// Supported delivery platforms.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum DeliveryPlatform {
    Api,
    Telegram,
    Discord,
    Slack,
    Webhook,
}

/// Routes agent output to the appropriate delivery platform.
pub struct DeliveryRouter;

impl DeliveryRouter {
    pub fn new() -> Self {
        Self
    }

    /// Route a message to the appropriate platform.
    pub async fn deliver(&self, platform: &DeliveryPlatform, user_id: &str, _message: &str) -> Result<(), String> {
        match platform {
            DeliveryPlatform::Api => {
                // API delivery is handled by WebSocket/SSE directly
                Ok(())
            }
            DeliveryPlatform::Telegram => {
                tracing::info!(user_id = user_id, "Would deliver to Telegram");
                Ok(())
            }
            DeliveryPlatform::Discord => {
                tracing::info!(user_id = user_id, "Would deliver to Discord");
                Ok(())
            }
            DeliveryPlatform::Slack => {
                tracing::info!(user_id = user_id, "Would deliver to Slack");
                Ok(())
            }
            DeliveryPlatform::Webhook => {
                tracing::info!(user_id = user_id, "Would deliver to Webhook");
                Ok(())
            }
        }
    }
}

impl Default for DeliveryRouter {
    fn default() -> Self {
        Self::new()
    }
}
