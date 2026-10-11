//! Deployment-level key/value settings stored in the main SQLite database.
//!
//! Holds admin-managed runtime configuration (today: the mail channel set
//! through `/api/admin/mail`) that must survive restarts without an `.env`
//! file — the desktop bundle has none, which is exactly why the settings
//! live here rather than in the environment.

use anyhow::Result;
use sqlx::SqlitePool;

pub struct AppSettingsStore {
    pool: SqlitePool,
}

impl AppSettingsStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn get(&self, key: &str) -> Result<Option<String>> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT value FROM app_settings WHERE key = ?")
                .bind(key)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.map(|(v,)| v))
    }

    /// Insert or update `key` in place.
    pub async fn set(&self, key: &str, value: &str) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO app_settings (key, value)
            VALUES (?, ?)
            ON CONFLICT(key) DO UPDATE SET
                value = excluded.value,
                updated_at = datetime('now')
            "#,
        )
        .bind(key)
        .bind(value)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Delete `key`; missing keys are not an error.
    pub async fn remove(&self, key: &str) -> Result<()> {
        sqlx::query("DELETE FROM app_settings WHERE key = ?")
            .bind(key)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn store() -> (tempfile::TempDir, AppSettingsStore) {
        let dir = tempfile::tempdir().unwrap();
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect(&format!("sqlite:{}?mode=rwc", dir.path().join("t.db").to_str().unwrap()))
            .await
            .unwrap();
        crate::migration::run_migrations(&pool).await.unwrap();
        (dir, AppSettingsStore::new(pool))
    }

    #[tokio::test]
    async fn set_get_remove_round_trip() {
        let (_dir, s) = store().await;

        assert_eq!(s.get("mail").await.unwrap(), None, "missing key reads as None");

        s.set("mail", r#"{"provider":"smtp"}"#).await.unwrap();
        assert_eq!(s.get("mail").await.unwrap().as_deref(), Some(r#"{"provider":"smtp"}"#));

        // Overwrite updates in place rather than duplicating.
        s.set("mail", r#"{"provider":"dev"}"#).await.unwrap();
        assert_eq!(s.get("mail").await.unwrap().as_deref(), Some(r#"{"provider":"dev"}"#));

        s.remove("mail").await.unwrap();
        assert_eq!(s.get("mail").await.unwrap(), None);
        // Removing a missing key is fine (idempotent).
        s.remove("mail").await.unwrap();
    }
}
