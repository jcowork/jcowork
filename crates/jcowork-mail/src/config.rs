//! Environment-driven email configuration and channel selection.
//!
//! Mirrors `jcowork-sms`: deployment-level credentials read from the
//! environment once at startup, with a dev fallback when SMTP is unconfigured.

use std::sync::Arc;

use crate::dev::DevMailSender;
use crate::provider::MailSender;
use crate::smtp::SmtpMailSender;

/// Which channel `auto` resolved to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailProviderKind {
    Smtp,
    Dev,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MailConfig {
    /// `auto` (default) uses SMTP when host/username/password/from are all
    /// present and silently falls back to dev otherwise; `smtp` fails fast at
    /// startup if any is missing; `dev` never sends a real message.
    pub provider: String,
    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_username: String,
    pub smtp_password: String,
    pub smtp_from: String,
    /// Process-wide ceiling on messages sent per UTC day (toll/abuse guard,
    /// independent of the SMS cap).
    pub daily_global_cap: u32,
    /// When false, the email registration endpoint rejects new accounts,
    /// leaving SMS (or admin phone assignment) as the only sign-up path.
    pub allow_register: bool,
}

impl Default for MailConfig {
    fn default() -> Self {
        Self {
            provider: "auto".to_string(),
            smtp_host: String::new(),
            smtp_port: 587,
            smtp_username: String::new(),
            smtp_password: String::new(),
            smtp_from: String::new(),
            daily_global_cap: 500,
            allow_register: true,
        }
    }
}

impl MailConfig {
    pub fn from_env() -> Self {
        let var = |key: &str| std::env::var(key).unwrap_or_default();
        Self {
            provider: std::env::var("JCWORK_MAIL_PROVIDER")
                .unwrap_or_else(|_| "auto".to_string())
                .to_lowercase(),
            smtp_host: var("JCWORK_SMTP_HOST"),
            smtp_port: std::env::var("JCWORK_SMTP_PORT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(587),
            smtp_username: var("JCWORK_SMTP_USERNAME"),
            smtp_password: var("JCWORK_SMTP_PASSWORD"),
            smtp_from: var("JCWORK_SMTP_FROM"),
            daily_global_cap: std::env::var("JCWORK_MAIL_DAILY_GLOBAL_CAP")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(500),
            allow_register: std::env::var("JCWORK_MAIL_ALLOW_REGISTER")
                .ok()
                .map(|v| !matches!(v.trim().to_lowercase().as_str(), "0" | "false" | "no"))
                .unwrap_or(true),
        }
    }

    fn has_smtp_credentials(&self) -> bool {
        !self.smtp_host.is_empty()
            && !self.smtp_username.is_empty()
            && !self.smtp_password.is_empty()
            && !self.smtp_from.is_empty()
    }

    /// True when `provider` is `auto` (or unknown) but no SMTP credentials are
    /// present, so the channel silently fell back to dev.
    ///
    /// An explicit `provider = "dev"` is an operator's deliberate choice and
    /// does not count: the admin setup gate keys on this so it never locks
    /// someone who already declared "no mail".
    pub fn is_auto_dev(&self) -> bool {
        !self.provider.eq_ignore_ascii_case("dev")
            && self.resolve().map(|k| k == MailProviderKind::Dev).unwrap_or(false)
    }

    /// Resolve `provider` into a concrete channel.
    ///
    /// `smtp` with missing credentials is a deployment error: a silent
    /// fallback to dev would look exactly like an SMTP outage to anyone
    /// waiting for a code.
    pub fn resolve(&self) -> anyhow::Result<MailProviderKind> {
        match self.provider.as_str() {
            "dev" => Ok(MailProviderKind::Dev),
            "smtp" => {
                if self.has_smtp_credentials() {
                    Ok(MailProviderKind::Smtp)
                } else {
                    anyhow::bail!(
                        "JCWORK_MAIL_PROVIDER=smtp but JCWORK_SMTP_HOST / _PORT / \
                         _USERNAME / _PASSWORD / _FROM are not all set"
                    )
                }
            }
            _ if self.has_smtp_credentials() => Ok(MailProviderKind::Smtp),
            _ => Ok(MailProviderKind::Dev),
        }
    }

    /// Build the channel selected by [`resolve`](Self::resolve).
    pub fn build(&self) -> anyhow::Result<Arc<dyn MailSender>> {
        match self.resolve()? {
            MailProviderKind::Smtp => Ok(Arc::new(SmtpMailSender::new(
                &self.smtp_host,
                self.smtp_port,
                &self.smtp_username,
                &self.smtp_password,
                &self.smtp_from,
            )?)),
            MailProviderKind::Dev => Ok(Arc::new(DevMailSender)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full() -> MailConfig {
        MailConfig {
            smtp_host: "smtp.example.com".into(),
            smtp_username: "u".into(),
            smtp_password: "p".into(),
            smtp_from: "noreply@example.com".into(),
            ..MailConfig::default()
        }
    }

    /// One test covers every env-var permutation on purpose: `std::env::set_var`
    /// is process-global, so parallel `#[test]`s would observe each other's
    /// values (same reasoning as the merged JCWORK_SMS cases).
    #[test]
    fn resolve_and_from_env() {
        // Explicit dev wins even with credentials present.
        assert_eq!(
            MailConfig { provider: "dev".into(), ..full() }.resolve().unwrap(),
            MailProviderKind::Dev
        );
        // Auto picks SMTP only when host/username/password/from are all set.
        assert_eq!(full().resolve().unwrap(), MailProviderKind::Smtp);
        assert_eq!(
            MailConfig::default().resolve().unwrap(),
            MailProviderKind::Dev
        );
        assert_eq!(
            MailConfig { smtp_from: String::new(), ..full() }.resolve().unwrap(),
            MailProviderKind::Dev
        );
        // Explicit smtp with missing credentials is a hard error, not a
        // silent downgrade.
        assert!(MailConfig {
            provider: "smtp".into(),
            ..MailConfig::default()
        }
        .resolve()
        .is_err());

        // Unknown provider behaves like auto.
        assert_eq!(
            MailConfig { provider: "carrier-pigeon".into(), ..full() }.resolve().unwrap(),
            MailProviderKind::Smtp
        );

        unsafe { std::env::set_var("JCWORK_MAIL_PROVIDER", "DEV"); }
        unsafe { std::env::set_var("JCWORK_SMTP_PORT", "465"); }
        unsafe { std::env::set_var("JCWORK_MAIL_DAILY_GLOBAL_CAP", "42"); }
        unsafe { std::env::set_var("JCWORK_MAIL_ALLOW_REGISTER", "false"); }
        let cfg = MailConfig::from_env();
        assert_eq!(cfg.provider, "dev");
        assert_eq!(cfg.smtp_port, 465);
        assert_eq!(cfg.daily_global_cap, 42);
        assert!(!cfg.allow_register);
        unsafe { std::env::remove_var("JCWORK_MAIL_PROVIDER"); }
        unsafe { std::env::remove_var("JCWORK_SMTP_PORT"); }
        unsafe { std::env::remove_var("JCWORK_MAIL_DAILY_GLOBAL_CAP"); }
        unsafe { std::env::remove_var("JCWORK_MAIL_ALLOW_REGISTER"); }

        let cfg = MailConfig::from_env();
        assert_eq!(cfg.provider, "auto");
        assert_eq!(cfg.smtp_port, 587);
        assert_eq!(cfg.daily_global_cap, 500);
        assert!(cfg.allow_register);

        // An unparseable cap or port falls back to the default.
        unsafe { std::env::set_var("JCWORK_MAIL_DAILY_GLOBAL_CAP", "lots"); }
        unsafe { std::env::set_var("JCWORK_SMTP_PORT", "not-a-port"); }
        let cfg = MailConfig::from_env();
        assert_eq!(cfg.daily_global_cap, 500);
        assert_eq!(cfg.smtp_port, 587);
        unsafe { std::env::remove_var("JCWORK_MAIL_DAILY_GLOBAL_CAP"); }
        unsafe { std::env::remove_var("JCWORK_SMTP_PORT"); }
    }

    #[test]
    fn build_produces_a_named_channel() {
        assert_eq!(MailConfig::default().build().unwrap().name(), "dev");
        assert_eq!(full().build().unwrap().name(), "smtp");
    }

    #[test]
    fn is_auto_dev_distinguishes_unconfigured_from_declared_dev() {
        // auto without credentials = unconfigured.
        assert!(MailConfig::default().is_auto_dev());
        // auto with credentials = configured.
        assert!(!full().is_auto_dev());
        // explicit dev = a deliberate choice, never "unconfigured".
        assert!(!MailConfig { provider: "dev".into(), ..MailConfig::default() }.is_auto_dev());
        assert!(!MailConfig { provider: "DEV".into(), ..MailConfig::default() }.is_auto_dev());
        // unknown provider behaves like auto.
        assert!(MailConfig {
            provider: "carrier-pigeon".into(),
            ..MailConfig::default()
        }
        .is_auto_dev());
        // smtp with missing credentials is a config error, not "unconfigured".
        assert!(!MailConfig { provider: "smtp".into(), ..MailConfig::default() }.is_auto_dev());
    }

    #[test]
    fn config_round_trips_through_json() {
        let cfg = MailConfig {
            provider: "smtp".into(),
            smtp_host: "smtp.example.com".into(),
            smtp_port: 465,
            smtp_username: "u".into(),
            smtp_password: "secret".into(),
            smtp_from: "noreply@example.com".into(),
            daily_global_cap: 42,
            allow_register: false,
        };
        let json = serde_json::to_string(&cfg).unwrap();
        assert!(!json.contains("smtp_password") == false, "password must serialize (stored config)");
        let back: MailConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.smtp_host, cfg.smtp_host);
        assert_eq!(back.smtp_password, "secret");
        assert_eq!(back.daily_global_cap, 42);
        assert!(!back.allow_register);
    }
}
