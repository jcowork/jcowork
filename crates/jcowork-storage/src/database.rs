//! Database abstraction with per-user SQLite connection pools.

use anyhow::Result;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions, SqliteJournalMode, SqliteLockingMode};
use sqlx::SqlitePool;
use std::str::FromStr;
use tracing::info;

/// Manages per-user SQLite database connections.
///
/// Each user gets their own SQLite file with WAL mode for concurrent reads.
/// Database files are stored under `data_dir/<user_id>/jcowork.db`.
#[derive(Debug)]
pub struct Database {
    data_dir: String,
}

impl Database {
    /// Create a new Database manager rooted at `data_dir`.
    pub fn new(data_dir: &str) -> Self {
        Self {
            data_dir: data_dir.to_string(),
        }
    }

    /// Get or create a SQLite connection pool for a specific user.
    pub async fn get_pool(&self, user_id: &str) -> Result<SqlitePool> {
        let user_dir = format!("{}/{}", self.data_dir, user_id);
        tokio::fs::create_dir_all(&user_dir).await?;

        let db_path = format!("{}/jcowork.db", user_dir);
        let db_url = format!("sqlite:{}?mode=rwc", db_path);

        let options = SqliteConnectOptions::from_str(&db_url)?
            .journal_mode(SqliteJournalMode::Wal)
            .locking_mode(SqliteLockingMode::Normal)
            .busy_timeout(std::time::Duration::from_secs(5))
            .foreign_keys(true);

        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await?;

        // Run migrations
        crate::migration::run_migrations(&pool).await?;

        info!(user_id = user_id, "Created database pool");
        Ok(pool)
    }

    /// Get the data directory path.
    pub fn data_dir(&self) -> &str {
        &self.data_dir
    }

    /// Get user workspace directory path.
    pub fn user_workspace(&self, user_id: &str) -> String {
        format!("{}/{}/workspace", self.data_dir, user_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_database_creation() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(dir.path().to_str().unwrap());
        let pool = db.get_pool("test-user").await;
        assert!(pool.is_ok());
    }
}
