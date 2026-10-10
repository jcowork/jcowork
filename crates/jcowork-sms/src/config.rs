//! Environment-driven SMS configuration and channel selection.
//!
//! Unlike `jcowork-feishu`'s per-user database config, SMS credentials are
//! deployment-level (one operator account pays for every user), so they are
//! read from the environment once at startup.

use std::sync::Arc;

use crate::aliyun::AliyunSmsSender;
use crate::dev::DevSmsSender;
use crate::provider::SmsSender;

/// Which channel `auto` resolved to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmsProviderKind {
    Aliyun,
    Dev,
}

#[derive(Debug, Clone)]
pub struct SmsConfig {
    /// `auto` (default) uses Aliyun when all four credentials are present and
    /// silently falls back to dev otherwise; `aliyun` fails fast at startup if
    /// credentials are missing; `dev` never sends a real message.
    pub provider: String,
    pub access_key_id: String,
    pub access_key_secret: String,
    pub sign_name: String,
    pub template_code: String,
    /// Process-wide ceiling on messages sent per UTC day. This is the only
    /// effective guard against toll fraud: per-phone throttling cannot stop an
    /// attacker enumerating the number space and draining the SMS balance.
    pub daily_global_cap: u32,
    /// When false, an unknown phone number is rejected instead of being
    /// auto-provisioned, turning SMS login into an invite-only gate.
    pub allow_register: bool,
}

impl Default for SmsConfig {
    fn default() -> Self {
        Self {
            provider: "auto".to_string(),
            access_key_id: String::new(),
            access_key_secret: String::new(),
            sign_name: String::new(),
            template_code: String::new(),
            daily_global_cap: 500,
            allow_register: true,
        }
    }
}

impl SmsConfig {
    pub fn from_env() -> Self {
        let var = |key: &str| std::env::var(key).unwrap_or_default();
        Self {
            provider: std::env::var("JCWORK_SMS_PROVIDER")
                .unwrap_or_else(|_| "auto".to_string())
                .to_lowercase(),
            access_key_id: var("JCWORK_SMS_ACCESS_KEY_ID"),
            access_key_secret: var("JCWORK_SMS_ACCESS_KEY_SECRET"),
            sign_name: var("JCWORK_SMS_SIGN_NAME"),
            template_code: var("JCWORK_SMS_TEMPLATE_CODE"),
            daily_global_cap: std::env::var("JCWORK_SMS_DAILY_GLOBAL_CAP")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(500),
            allow_register: std::env::var("JCWORK_SMS_ALLOW_REGISTER")
                .ok()
                .map(|v| !matches!(v.trim().to_lowercase().as_str(), "0" | "false" | "no"))
                .unwrap_or(true),
        }
    }

    fn has_aliyun_credentials(&self) -> bool {
        !self.access_key_id.is_empty()
            && !self.access_key_secret.is_empty()
            && !self.sign_name.is_empty()
            && !self.template_code.is_empty()
    }

    /// Resolve `provider` into a concrete channel.
    ///
    /// `aliyun` with missing credentials is a deployment error and surfaces as
    /// `Err` rather than silently degrading to dev, which would otherwise let
    /// an operator believe real messages are going out.
    pub fn resolve(&self) -> anyhow::Result<SmsProviderKind> {
        match self.provider.as_str() {
            "dev" => Ok(SmsProviderKind::Dev),
            "aliyun" => {
                if self.has_aliyun_credentials() {
                    Ok(SmsProviderKind::Aliyun)
                } else {
                    anyhow::bail!(
                        "JCWORK_SMS_PROVIDER=aliyun but JCWORK_SMS_ACCESS_KEY_ID / \
                         _ACCESS_KEY_SECRET / _SIGN_NAME / _TEMPLATE_CODE are not all set"
                    )
                }
            }
            _ if self.has_aliyun_credentials() => Ok(SmsProviderKind::Aliyun),
            _ => Ok(SmsProviderKind::Dev),
        }
    }

    /// Build the channel selected by [`resolve`](Self::resolve).
    pub fn build(&self) -> anyhow::Result<Arc<dyn SmsSender>> {
        match self.resolve()? {
            SmsProviderKind::Aliyun => Ok(Arc::new(AliyunSmsSender::new(
                self.access_key_id.clone(),
                self.access_key_secret.clone(),
                self.sign_name.clone(),
                self.template_code.clone(),
            )?)),
            SmsProviderKind::Dev => Ok(Arc::new(DevSmsSender)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full() -> SmsConfig {
        SmsConfig {
            access_key_id: "id".into(),
            access_key_secret: "secret".into(),
            sign_name: "sign".into(),
            template_code: "SMS_1".into(),
            ..SmsConfig::default()
        }
    }

    /// One test covers every env-var permutation on purpose: `std::env::set_var`
    /// is process-global, so parallel `#[test]`s would observe each other's
    /// values (same reasoning as the merged JCWORK_SCRIPTS_DIR cases).
    #[test]
    fn resolve_and_from_env() {
        // Explicit dev wins even with credentials present.
        assert_eq!(
            SmsConfig { provider: "dev".into(), ..full() }.resolve().unwrap(),
            SmsProviderKind::Dev
        );
        // Auto picks Aliyun only when all four credentials are set.
        assert_eq!(full().resolve().unwrap(), SmsProviderKind::Aliyun);
        assert_eq!(
            SmsConfig::default().resolve().unwrap(),
            SmsProviderKind::Dev
        );
        assert_eq!(
            SmsConfig { template_code: String::new(), ..full() }
                .resolve()
                .unwrap(),
            SmsProviderKind::Dev
        );
        // Explicit aliyun with missing credentials is a hard error, not a
        // silent downgrade.
        assert!(SmsConfig {
            provider: "aliyun".into(),
            ..SmsConfig::default()
        }
        .resolve()
        .is_err());

        // Unknown provider behaves like auto.
        assert_eq!(
            SmsConfig { provider: "carrier-pigeon".into(), ..full() }
                .resolve()
                .unwrap(),
            SmsProviderKind::Aliyun
        );

        unsafe { std::env::set_var("JCWORK_SMS_PROVIDER", "DEV"); }
        unsafe { std::env::set_var("JCWORK_SMS_DAILY_GLOBAL_CAP", "42"); }
        unsafe { std::env::set_var("JCWORK_SMS_ALLOW_REGISTER", "false"); }
        let cfg = SmsConfig::from_env();
        assert_eq!(cfg.provider, "dev");
        assert_eq!(cfg.daily_global_cap, 42);
        assert!(!cfg.allow_register);
        unsafe { std::env::remove_var("JCWORK_SMS_PROVIDER"); }
        unsafe { std::env::remove_var("JCWORK_SMS_DAILY_GLOBAL_CAP"); }
        unsafe { std::env::remove_var("JCWORK_SMS_ALLOW_REGISTER"); }

        let cfg = SmsConfig::from_env();
        assert_eq!(cfg.provider, "auto");
        assert_eq!(cfg.daily_global_cap, 500);
        assert!(cfg.allow_register);

        // An unparseable cap falls back to the default rather than panicking.
        unsafe { std::env::set_var("JCWORK_SMS_DAILY_GLOBAL_CAP", "lots"); }
        assert_eq!(SmsConfig::from_env().daily_global_cap, 500);
        unsafe { std::env::remove_var("JCWORK_SMS_DAILY_GLOBAL_CAP"); }
    }

    #[test]
    fn build_produces_a_named_channel() {
        assert_eq!(SmsConfig::default().build().unwrap().name(), "dev");
        assert_eq!(full().build().unwrap().name(), "aliyun");
    }
}
