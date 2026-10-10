//! Fallback channel that writes the code to the server log instead of sending
//! a real SMS. Used when no provider credentials are configured, so a fresh
//! checkout can exercise the whole login flow offline.

use anyhow::Result;
use async_trait::async_trait;

use crate::provider::SmsSender;

pub struct DevSmsSender;

#[async_trait]
impl SmsSender for DevSmsSender {
    async fn send_code(&self, phone: &str, code: &str) -> Result<()> {
        tracing::warn!(
            phone = %mask(phone),
            code = %code,
            "SMS dev channel: verification code printed to the SERVER LOG only. \
             Set JCWORK_SMS_ACCESS_KEY_ID / _SECRET / _SIGN_NAME / _TEMPLATE_CODE \
             to deliver real messages."
        );
        Ok(())
    }

    fn name(&self) -> &'static str {
        "dev"
    }
}

/// Show only the last four digits so a shared log stays private.
fn mask(phone: &str) -> String {
    let tail = &phone[phone.len().saturating_sub(4)..];
    format!("****{}", tail)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_keeps_only_last_four_digits() {
        assert_eq!(mask("13800138000"), "****8000");
        assert_eq!(mask("123"), "****123");
        assert_eq!(mask(""), "****");
    }

    #[tokio::test]
    async fn dev_sender_always_succeeds() {
        assert!(DevSmsSender.send_code("13800138000", "123456").await.is_ok());
    }
}
