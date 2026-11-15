//! Per-user preferences stored alongside the shared memory database.
//!
//! The web UI keeps a copy of the selected model in `localStorage`, but the
//! backend needs the same choice for conversations it starts on its own —
//! the Feishu bot answering a chat message has no browser to ask.

use anyhow::Result;
use sqlx::SqlitePool;

/// Per-user settings rows keyed by `user_id` (one row per account).
#[derive(Debug, Clone)]
pub struct UserSettingsStore {
    pool: SqlitePool,
}

impl UserSettingsStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// The model the user picked in Settings ("provider:model"), if any.
    pub async fn get_model(&self, user_id: &str) -> Result<Option<String>> {
        let row = sqlx::query_as::<_, (Option<String>,)>(
            "SELECT model FROM user_settings WHERE user_id = ?1",
        )
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.and_then(|(m,)| m).filter(|m| !m.trim().is_empty()))
    }

    /// Save (or clear, when `model` is `None`) the selected model.
    pub async fn set_model(&self, user_id: &str, model: Option<&str>) -> Result<()> {
        let model = model.map(str::trim).filter(|m| !m.is_empty());
        sqlx::query(
            r#"
            INSERT INTO user_settings (user_id, model, updated_at)
            VALUES (?1, ?2, datetime('now'))
            ON CONFLICT(user_id) DO UPDATE SET
                model = excluded.model,
                updated_at = datetime('now')
            "#,
        )
        .bind(user_id)
        .bind(model)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Whether the account opted into daily RSI (recursive self-improvement).
    /// Defaults to false: an account that never touched the nav switch must
    /// not start running background agent turns on upgrade.
    pub async fn get_rsi_enabled(&self, user_id: &str) -> Result<bool> {
        let row = sqlx::query_as::<_, (i64,)>(
            "SELECT rsi_enabled FROM user_settings WHERE user_id = ?1",
        )
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|(v,)| v != 0).unwrap_or(false))
    }

    /// Turn the daily RSI run on or off for an account. Only the flag is
    /// written, so the saved model choice is preserved.
    pub async fn set_rsi_enabled(&self, user_id: &str, enabled: bool) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO user_settings (user_id, rsi_enabled, updated_at)
            VALUES (?1, ?2, datetime('now'))
            ON CONFLICT(user_id) DO UPDATE SET
                rsi_enabled = excluded.rsi_enabled,
                updated_at = datetime('now')
            "#,
        )
        .bind(user_id)
        .bind(if enabled { 1_i64 } else { 0_i64 })
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Every account that opted into RSI — the daily scheduler's work list.
    pub async fn list_rsi_enabled_user_ids(&self) -> Result<Vec<String>> {
        let rows = sqlx::query_as::<_, (String,)>(
            "SELECT user_id FROM user_settings WHERE rsi_enabled = 1 ORDER BY user_id",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|(id,)| id).collect())
    }
}
