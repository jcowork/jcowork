//! SMS delivery abstraction for verification-code login.
//!
//! Pure third-party SDK layer: no axum, no sqlx, no AppState — mirrors the
//! split used by `jcowork-feishu`. HTTP handlers live in `jcowork-gateway`.

pub mod aliyun;
pub mod config;
pub mod dev;
pub mod provider;

pub use provider::SmsSender;
