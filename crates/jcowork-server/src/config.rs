//! Server configuration.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Global server configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    /// Server bind address.
    pub host: String,
    /// Server bind port.
    pub port: u16,
    /// Data directory for per-user databases and workspaces.
    pub data_dir: String,
    /// JWT secret for authentication.
    pub jwt_secret: String,
    /// JWT token duration in hours.
    pub token_duration_hours: i64,
    /// Default LLM model (format: "provider:model", e.g., "deepseek:deepseek-chat").
    pub default_model: String,
    /// User actor idle timeout in seconds.
    pub idle_timeout_secs: u64,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "0.0.0.0".to_string(),
            port: 3000,
            data_dir: default_data_dir(),
            jwt_secret: "change-me-in-production".to_string(),
            token_duration_hours: 24,
            default_model: "moonshot:kimi-k2.6".to_string(),
            idle_timeout_secs: 300,
        }
    }
}

impl ServerConfig {
    /// Load config from environment variables with defaults.
    pub fn from_env() -> Self {
        Self {
            host: std::env::var("JCWORK_HOST").unwrap_or_else(|_| "0.0.0.0".to_string()),
            port: std::env::var("JCWORK_PORT")
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or(3000),
            data_dir: std::env::var("JCWORK_DATA_DIR")
                .unwrap_or_else(|_| default_data_dir()),
            jwt_secret: std::env::var("JCWORK_JWT_SECRET")
                .unwrap_or_else(|_| "change-me-in-production".to_string()),
            token_duration_hours: std::env::var("JCWORK_TOKEN_DURATION_HOURS")
                .ok()
                .and_then(|h| h.parse().ok())
                .unwrap_or(24),
            default_model: std::env::var("JCWORK_DEFAULT_MODEL")
                .unwrap_or_else(|_| "moonshot:kimi-k2.6".to_string()),
            idle_timeout_secs: std::env::var("JCWORK_IDLE_TIMEOUT")
                .ok()
                .and_then(|t| t.parse().ok())
                .unwrap_or(300),
        }
    }
}

fn default_data_dir() -> String {
    dirs_home().join(".jcowork").join("data").to_str().unwrap_or("~/.jcowork/data").to_string()
}

fn dirs_home() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"))
}
