//! Generic SMTP sender via `lettre` (rustls, no openssl).
//!
//! Two TLS shapes exist in the wild and the port decides which one to use:
//! 465 is implicit TLS ("SMTPS", wrap the whole connection), while 587 (and
//! 25) use STARTTLS (upgrade after the plaintext greeting).

use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use lettre::{
    message::{Mailbox, SinglePart, header::ContentType},
    transport::smtp::authentication::Credentials,
    transport::smtp::client::Tls,
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
};

use crate::provider::MailSender;

/// Port that speaks implicit TLS rather than STARTTLS.
const SMTPS_PORT: u16 = 465;

pub struct SmtpMailSender {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    from: Mailbox,
}

impl SmtpMailSender {
    pub fn new(
        host: &str,
        port: u16,
        username: &str,
        password: &str,
        from: &str,
    ) -> Result<Self> {
        let from: Mailbox = from
            .parse()
            .map_err(|e| anyhow::anyhow!("invalid JCWORK_SMTP_FROM address: {e}"))?;

        let mut builder = if port == SMTPS_PORT {
            AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(host.to_string())
                .port(port)
                .tls(Tls::Wrapper(
                    lettre::transport::smtp::client::TlsParameters::new(host.to_string())
                        .map_err(|e| anyhow::anyhow!("TLS setup for {host} failed: {e}"))?,
                ))
        } else {
            AsyncSmtpTransport::<Tokio1Executor>::relay(host)
                .map_err(|e| anyhow::anyhow!("relay setup for {host} failed: {e}"))?
                .port(port)
        };

        builder = builder
            .credentials(Credentials::new(username.to_string(), password.to_string()))
            .timeout(Some(Duration::from_secs(20)));

        Ok(Self {
            transport: builder.build(),
            from,
        })
    }

    /// Compose and send a plain-text message. Both `send_code` and
    /// `send_message` funnel through here so the TLS/auth/timeout setup has
    /// exactly one delivery path.
    async fn send_raw(&self, email: &str, subject: &str, body: String) -> Result<()> {
        let to: Mailbox = email
            .parse()
            .map_err(|e| anyhow::anyhow!("invalid recipient address: {e}"))?;

        let mail = Message::builder()
            .from(self.from.clone())
            .to(to)
            .subject(subject)
            .singlepart(
                SinglePart::builder()
                    .header(ContentType::TEXT_PLAIN)
                    .body(body),
            )
            .map_err(|e| anyhow::anyhow!("failed to compose the email: {e}"))?;

        self.transport
            .send(mail)
            .await
            .map_err(|e| anyhow::anyhow!("SMTP send failed: {e}"))?;
        Ok(())
    }
}

#[async_trait]
impl MailSender for SmtpMailSender {
    async fn send_code(&self, email: &str, code: &str) -> Result<()> {
        self.send_raw(
            email,
            "Jcowork 验证码",
            format!("您的验证码是 {code}，5 分钟内有效。\n若非本人操作，请忽略本邮件。"),
        )
        .await
    }

    async fn send_message(&self, email: &str, subject: &str, body: &str) -> Result<()> {
        self.send_raw(email, subject, body.to_string()).await
    }

    fn name(&self) -> &'static str {
        "smtp"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_construction_never_touches_the_network() {
        let sender = SmtpMailSender::new("smtp.example.com", 587, "u", "p", "noreply@example.com");
        assert!(sender.is_ok());
        assert_eq!(sender.unwrap().name(), "smtp");

        let implicit = SmtpMailSender::new("smtp.example.com", 465, "u", "p", "noreply@example.com");
        assert!(implicit.is_ok());
    }

    #[test]
    fn rejects_malformed_from_address() {
        let bad = SmtpMailSender::new("smtp.example.com", 587, "u", "p", "not an address");
        assert!(bad.is_err());
    }

    #[test]
    fn display_name_in_from_is_accepted() {
        let ok = SmtpMailSender::new("smtp.example.com", 587, "u", "p", "Jcowork <noreply@example.com>");
        assert!(ok.is_ok());
    }
}
