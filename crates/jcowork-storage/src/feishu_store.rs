//! Per-user Feishu app configuration store.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

/// A Feishu app configuration entry.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct FeishuConfigEntry {
    pub user_id: String,
    pub app_id: String,
    pub app_secret: String,
    pub verification_token: String,
    pub encrypt_key: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Manages per-user Feishu app configurations.
#[derive(Debug, Clone)]
pub struct FeishuConfigStore {
    pool: SqlitePool,
}

impl FeishuConfigStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Save or update Feishu config for a user.
    pub async fn upsert(
        &self,
        user_id: &str,
        app_id: &str,
        app_secret: &str,
        verification_token: &str,
        encrypt_key: &str,
    ) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO feishu_configs (user_id, app_id, app_secret, verification_token, encrypt_key)
            VALUES (?, ?, ?, ?, ?)
            ON CONFLICT(user_id) DO UPDATE SET
                app_id = excluded.app_id,
                app_secret = excluded.app_secret,
                verification_token = excluded.verification_token,
                encrypt_key = excluded.encrypt_key,
                updated_at = datetime('now')
            "#,
        )
        .bind(user_id)
        .bind(app_id)
        .bind(app_secret)
        .bind(verification_token)
        .bind(encrypt_key)
        .execute(&self.pool)
        .await
        .map_err(|e| {
            if e.to_string().contains("UNIQUE constraint") {
                anyhow::anyhow!("App ID '{}' is already registered by another user", app_id)
            } else {
                anyhow::anyhow!("Failed to save Feishu config: {}", e)
            }
        })?;
        Ok(())
    }

    /// Get Feishu config by user_id.
    pub async fn get_by_user(&self, user_id: &str) -> Result<Option<FeishuConfigEntry>> {
        let entry = sqlx::query_as::<_, FeishuConfigEntry>(
            "SELECT user_id, app_id, app_secret, verification_token, encrypt_key, created_at, updated_at FROM feishu_configs WHERE user_id = ?",
        )
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(entry)
    }

    /// Get Feishu config by app_id (used by event handler to route events).
    pub async fn get_by_app_id(&self, app_id: &str) -> Result<Option<FeishuConfigEntry>> {
        let entry = sqlx::query_as::<_, FeishuConfigEntry>(
            "SELECT user_id, app_id, app_secret, verification_token, encrypt_key, created_at, updated_at FROM feishu_configs WHERE app_id = ?",
        )
        .bind(app_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(entry)
    }

    /// Delete Feishu config for a user.
    pub async fn delete(&self, user_id: &str) -> Result<()> {
        sqlx::query("DELETE FROM feishu_configs WHERE user_id = ?")
            .bind(user_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}
