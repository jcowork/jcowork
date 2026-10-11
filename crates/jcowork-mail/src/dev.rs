//! Fallback channel that writes the code to the server log instead of sending
//! a real email. Used when no SMTP credentials are configured, so a fresh
//! checkout can exercise the whole email login flow offline.

use anyhow::Result;
use async_trait::async_trait;

use crate::provider::MailSender;

pub struct DevMailSender;

#[async_trait]
impl MailSender for DevMailSender {
    async fn send_code(&self, email: &str, code: &str) -> Result<()> {
        tracing::warn!(
            email = %mask(email),
            code = %code,
            "Email dev channel: verification code printed to the SERVER LOG only. \
             Set JCWORK_SMTP_HOST / _PORT / _USERNAME / _PASSWORD / _FROM \
             to deliver real messages."
        );
        Ok(())
    }

    fn name(&self) -> &'static str {
        "dev"
    }
}

/// Hide the local part entirely and keep the domain, so a shared log stays
/// private while the operator can still tell providers apart.
fn mask(email: &str) -> String {
    match email.rsplit_once('@') {
        Some((_, domain)) => format!("****@{domain}"),
        None => "****".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_hides_local_part_and_keeps_domain() {
        assert_eq!(mask("alice@example.com"), "****@example.com");
        assert_eq!(mask("a@b.cn"), "****@b.cn");
        assert_eq!(mask("no-at-sign"), "****");
    }

    #[tokio::test]
    async fn dev_sender_always_succeeds() {
        assert!(DevMailSender.send_code("alice@example.com", "123456").await.is_ok());
    }
}
