//! Email delivery abstraction for verification codes.
//!
//! Pure third-party SDK layer: no axum, no sqlx, no AppState — mirrors the
//! split used by `jcowork-sms`. HTTP handlers live in `jcowork-gateway`.

pub mod config;
pub mod dev;
pub mod provider;
pub mod smtp;

pub use provider::MailSender;
