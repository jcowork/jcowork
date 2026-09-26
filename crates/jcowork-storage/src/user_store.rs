//! Global user account store.
//!
//! Uses a dedicated SQLite database (`users.db`) separate from per-user databases.
//! This allows user lookup before a per-user DB is initialized.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions, SqliteJournalMode, SqliteLockingMode};
use sqlx::SqlitePool;
use std::str::FromStr;

/// Recycle-bin retention: trashed accounts are permanently purged this many
/// days after being moved to the trash.
pub const TRASH_RETENTION_DAYS: i64 = 7;

/// A registered user.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct User {
    pub id: String,
    pub username: String,
    pub password_hash: String,
    pub feishu_open_id: Option<String>,
    pub created_at: String,
    /// Public account flag (chosen at registration). Public accounts are
    /// visible in every user's contacts list and their public documents /
    /// periodic-task results are readable by other users.
    pub is_public: bool,
    /// Super-user flag. The admin can manage users (search / delete /
    /// restore) through the user management page.
    pub is_admin: bool,
    /// Recycle-bin timestamp. `None` = active account; `Some(timestamp)` =
    /// moved to trash. Trashed accounts cannot log in and are invisible to
    /// other users; they are purged permanently 7 days after deletion.
    pub deleted_at: Option<String>,
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
                feishu_open_id TEXT UNIQUE,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                is_public INTEGER NOT NULL DEFAULT 0,
                is_admin INTEGER NOT NULL DEFAULT 0,
                deleted_at TEXT
            )
            "#,
        )
        .execute(&pool)
        .await?;

        // Add feishu_open_id column if it doesn't exist (migration for existing DBs)
        // Note: SQLite ALTER TABLE ADD COLUMN doesn't support UNIQUE constraint,
        // so we add without UNIQUE and create the index separately.
        let _ = sqlx::query(
            "ALTER TABLE users ADD COLUMN feishu_open_id TEXT",
        )
        .execute(&pool)
        .await; // Ignore error if column already exists

        // Add is_public column if it doesn't exist (migration for existing DBs).
        // Existing accounts default to private; the flag is chosen at registration.
        let _ = sqlx::query(
            "ALTER TABLE users ADD COLUMN is_public INTEGER NOT NULL DEFAULT 0",
        )
        .execute(&pool)
        .await; // Ignore error if column already exists

        // Add is_admin column if it doesn't exist (migration for existing DBs).
        // Only the seeded "admin" account is marked as super user.
        let _ = sqlx::query(
            "ALTER TABLE users ADD COLUMN is_admin INTEGER NOT NULL DEFAULT 0",
        )
        .execute(&pool)
        .await; // Ignore error if column already exists

        // Add deleted_at column if it doesn't exist (migration for existing DBs).
        // NULL = active; a timestamp means the account is in the recycle bin.
        let _ = sqlx::query(
            "ALTER TABLE users ADD COLUMN deleted_at TEXT",
        )
        .execute(&pool)
        .await; // Ignore error if column already exists

        let _ = sqlx::query(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_users_feishu_open_id ON users(feishu_open_id)",
        )
        .execute(&pool)
        .await; // Ignore error if index already exists

        Ok(Self { pool })
    }

    /// Create a new user. Returns error if username already exists.
    /// `is_public` is chosen at registration and cannot be changed afterwards.
    pub async fn create_user(&self, username: &str, password_hash: &str, is_public: bool) -> Result<User> {
        let id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO users (id, username, password_hash, is_public) VALUES (?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(username)
        .bind(password_hash)
        .bind(is_public)
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
            feishu_open_id: None,
            created_at: chrono::Utc::now().naive_utc().to_string(),
            is_public,
            is_admin: false,
            deleted_at: None,
        })
    }

    /// Look up a user by username.
    pub async fn get_user_by_username(&self, username: &str) -> Result<Option<User>> {
        let user = sqlx::query_as::<_, User>(
            "SELECT id, username, password_hash, feishu_open_id, created_at, is_public, is_admin, deleted_at FROM users WHERE username = ?",
        )
        .bind(username)
        .fetch_optional(&self.pool)
        .await?;

        Ok(user)
    }

    /// Look up a user by ID.
    pub async fn get_user_by_id(&self, user_id: &str) -> Result<Option<User>> {
        let user = sqlx::query_as::<_, User>(
            "SELECT id, username, password_hash, feishu_open_id, created_at, is_public, is_admin, deleted_at FROM users WHERE id = ?",
        )
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(user)
    }

    /// List all public accounts, ordered by username.
    /// Trashed (recycle-bin) accounts are excluded: they are invisible to
    /// other users while in the trash.
    /// Callers must only expose safe fields (never `password_hash`).
    pub async fn list_public_users(&self) -> Result<Vec<User>> {
        let users = sqlx::query_as::<_, User>(
            "SELECT id, username, password_hash, feishu_open_id, created_at, is_public, is_admin, deleted_at FROM users WHERE is_public = 1 AND deleted_at IS NULL ORDER BY username",
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(users)
    }

    /// Update a user's password hash.
    pub async fn update_password_hash(&self, user_id: &str, new_hash: &str) -> Result<()> {
        let result = sqlx::query("UPDATE users SET password_hash = ? WHERE id = ?")
            .bind(new_hash)
            .bind(user_id)
            .execute(&self.pool)
            .await?;

        if result.rows_affected() == 0 {
            anyhow::bail!("User not found");
        }
        Ok(())
    }

    /// Look up a user by Feishu open_id.
    pub async fn get_user_by_feishu_open_id(&self, open_id: &str) -> Result<Option<User>> {
        let user = sqlx::query_as::<_, User>(
            "SELECT id, username, password_hash, feishu_open_id, created_at, is_public, is_admin, deleted_at FROM users WHERE feishu_open_id = ?",
        )
        .bind(open_id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(user)
    }

    /// Get or create a jcowork user for a Feishu open_id.
    /// If the user doesn't exist, auto-creates one with a random password.
    pub async fn get_or_create_by_feishu_id(&self, open_id: &str) -> Result<User> {
        // Check if user already exists
        if let Some(user) = self.get_user_by_feishu_open_id(open_id).await? {
            return Ok(user);
        }

        // Auto-create user with a placeholder password (Feishu users don't log in via web)
        let short_id = &open_id[..open_id.len().min(8)];
        let username = format!("feishu_{}", short_id);
        let password_hash = format!("!feishu-auth-only!:{}", uuid::Uuid::new_v4());

        let id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO users (id, username, password_hash, feishu_open_id) VALUES (?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(&username)
        .bind(&password_hash)
        .bind(open_id)
        .execute(&self.pool)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to create Feishu user: {}", e))?;

        Ok(User {
            id,
            username,
            password_hash,
            feishu_open_id: Some(open_id.to_string()),
            created_at: chrono::Utc::now().naive_utc().to_string(),
            is_public: false,
            is_admin: false,
            deleted_at: None,
        })
    }

    // ── Admin user management (super user) ──

    /// Ensure an admin account exists for `username`.
    ///
    /// If the username is not taken, creates it with `is_admin = 1`.
    /// If it already exists, promotes it to admin. Idempotent — safe to call
    /// on every startup. Returns true when the account was created.
    pub async fn ensure_admin_user(&self, username: &str, password_hash: &str) -> Result<bool> {
        if let Some(existing) = self.get_user_by_username(username).await? {
            sqlx::query("UPDATE users SET is_admin = 1 WHERE id = ?")
                .bind(&existing.id)
                .execute(&self.pool)
                .await?;
            return Ok(false);
        }

        let id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO users (id, username, password_hash, is_admin) VALUES (?, ?, ?, 1)",
        )
        .bind(&id)
        .bind(username)
        .bind(password_hash)
        .execute(&self.pool)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to create admin user: {}", e))?;

        Ok(true)
    }

    /// List users for the admin management page.
    ///
    /// `status`: "active" (not in trash), "trash" (in trash), anything else
    /// = all users. `query` filters by username substring (case-insensitive).
    /// Callers must only expose safe fields (never `password_hash`).
    pub async fn list_users(&self, query: Option<&str>, status: &str) -> Result<Vec<User>> {
        let status_clause = match status {
            "active" => " AND deleted_at IS NULL",
            "trash" => " AND deleted_at IS NOT NULL",
            _ => "",
        };
        let search = query.map(|q| format!("%{}%", q.trim().to_lowercase()));
        let sql = format!(
            "SELECT id, username, password_hash, feishu_open_id, created_at, is_public, is_admin, deleted_at \
             FROM users WHERE 1=1{}{} ORDER BY created_at DESC",
            status_clause,
            if search.is_some() { " AND LOWER(username) LIKE ?" } else { "" },
        );

        let mut q = sqlx::query_as::<_, User>(&sql);
        if let Some(s) = &search {
            q = q.bind(s);
        }
        let users = q.fetch_all(&self.pool).await?;
        Ok(users)
    }

    /// Move a user to the recycle bin (soft delete).
    /// Fails if the user is missing or already trashed.
    pub async fn soft_delete_user(&self, user_id: &str) -> Result<()> {
        let result = sqlx::query(
            "UPDATE users SET deleted_at = datetime('now') WHERE id = ? AND deleted_at IS NULL",
        )
        .bind(user_id)
        .execute(&self.pool)
        .await?;

        if result.rows_affected() == 0 {
            anyhow::bail!("User not found or already deleted");
        }
        Ok(())
    }

    /// Restore a user from the recycle bin.
    /// Fails if the user is missing or not in the trash.
    pub async fn restore_user(&self, user_id: &str) -> Result<()> {
        let result = sqlx::query(
            "UPDATE users SET deleted_at = NULL WHERE id = ? AND deleted_at IS NOT NULL",
        )
        .bind(user_id)
        .execute(&self.pool)
        .await?;

        if result.rows_affected() == 0 {
            anyhow::bail!("User not found or not in trash");
        }
        Ok(())
    }

    /// Permanently delete a user account record.
    ///
    /// Only the `users` table row is removed — the per-user data directory
    /// ({data_dir}/{user_id}/) is intentionally left on disk.
    pub async fn permanently_delete_user(&self, user_id: &str) -> Result<()> {
        let result = sqlx::query("DELETE FROM users WHERE id = ?")
            .bind(user_id)
            .execute(&self.pool)
            .await?;

        if result.rows_affected() == 0 {
            anyhow::bail!("User not found");
        }
        Ok(())
    }

    /// Permanently delete all accounts whose recycle-bin timestamp is older
    /// than `days` days. Returns the number of purged accounts.
    pub async fn purge_expired_trash(&self, days: i64) -> Result<u64> {
        let result = sqlx::query(
            "DELETE FROM users WHERE deleted_at IS NOT NULL AND deleted_at <= datetime('now', ?)",
        )
        .bind(format!("-{} days", days))
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected())
    }
}
