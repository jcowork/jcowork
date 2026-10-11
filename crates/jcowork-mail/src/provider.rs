//! The pluggable email delivery contract.

use anyhow::Result;
use async_trait::async_trait;

/// Delivers a verification code to an email address.
///
/// Implementations must be safe to share across requests (`Arc<dyn MailSender>`
/// lives in the gateway's `AppState`).
#[async_trait]
pub trait MailSender: Send + Sync {
    /// Send `code` to `email`. `email` is already normalized (lowercased,
    /// syntax-checked) by the caller.
    ///
    /// Returning `Err` means the code was NOT delivered; callers must not
    /// accept a registration or password reset for a send that failed.
    async fn send_code(&self, email: &str, code: &str) -> Result<()>;

    /// Send an arbitrary message (e.g. the registration-credentials mail).
    /// Same delivery contract as [`MailSender::send_code`]: `Err` means the
    /// message was NOT delivered.
    async fn send_message(&self, email: &str, subject: &str, body: &str) -> Result<()>;

    /// Stable channel identifier, used in the startup banner and logs.
    fn name(&self) -> &'static str;
}
