//! The pluggable SMS delivery contract.

use anyhow::Result;
use async_trait::async_trait;

/// Delivers a verification code to a phone number.
///
/// Implementations must be safe to share across requests (`Arc<dyn SmsSender>`
/// lives in the gateway's `AppState`).
#[async_trait]
pub trait SmsSender: Send + Sync {
    /// Send `code` to `phone`. `phone` is already normalized to digits only.
    ///
    /// Returning `Err` means the code was NOT delivered; callers must not
    /// accept a login for a send that failed.
    async fn send_code(&self, phone: &str, code: &str) -> Result<()>;

    /// Stable channel identifier, used in the startup banner and logs.
    fn name(&self) -> &'static str;
}
