//! Global user account store.
//!
//! Uses a dedicated SQLite database (`users.db`) separate from per-user databases.
//! This allows user lookup before a per-user DB is initialized.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions, SqliteJournalMode, SqliteLockingMode};
use sqlx::SqlitePool;
use std::str::FromStr;

/// A registered user.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct User {
    pub id: String,
    pub username: String,
    pub password_hash: String,
    pub created_at: String,
}

/// Manages the global user accounts database.
#[derive(Debug, Clone)]
pub struct UserStore {
    pool: SqlitePool,
}

impl UserStore {
    /// Create a new UserStore rooted at `data_dir`.
    /// Initializes the `users.db` database and runs migrations.
    pub async fn new(data_dir: &str) -> Result<Self> {
        tokio::fs::create_dir_all(data_dir).await?;

        let db_path = format!("{}/users.db", data_dir);
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
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS users (
                id TEXT PRIMARY KEY,
                username TEXT NOT NULL UNIQUE,
                password_hash TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            )
            "#,
        )
        .execute(&pool)
        .await?;

        Ok(Self { pool })
    }

    /// Create a new user. Returns error if username already exists.
    pub async fn create_user(&self, username: &str, password_hash: &str) -> Result<User> {
        let id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO users (id, username, password_hash) VALUES (?, ?, ?)",
        )
        .bind(&id)
        .bind(username)
        .bind(password_hash)
        .execute(&self.pool)
        .await
        .map_err(|e| {
            if e.to_string().contains("UNIQUE constraint") {
                anyhow::anyhow!("Username '{}' already exists", username)
            } else {
                anyhow::anyhow!("Failed to create user: {}", e)
            }
        })?;

        Ok(User {
            id,
            username: username.to_string(),
            password_hash: password_hash.to_string(),
            created_at: chrono::Utc::now().naive_utc().to_string(),
        })
    }

    /// Look up a user by username.
    pub async fn get_user_by_username(&self, username: &str) -> Result<Option<User>> {
        let user = sqlx::query_as::<_, User>(
            "SELECT id, username, password_hash, created_at FROM users WHERE username = ?",
        )
        .bind(username)
        .fetch_optional(&self.pool)
        .await?;

        Ok(user)
    }

    /// Look up a user by ID.
    pub async fn get_user_by_id(&self, user_id: &str) -> Result<Option<User>> {
        let user = sqlx::query_as::<_, User>(
            "SELECT id, username, password_hash, created_at FROM users WHERE id = ?",
        )
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(user)
    }
}
