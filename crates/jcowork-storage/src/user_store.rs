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
    /// Phone number used for SMS verification-code login. `None` means the
    /// account has not bound one yet and therefore cannot log in by SMS.
    /// Unique across accounts; stored normalized to digits only.
    pub phone: Option<String>,
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
                phone TEXT,
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

        // Add phone column if it doesn't exist (migration for existing DBs).
        // Pre-existing accounts have no phone, so they cannot log in by SMS
        // until an admin assigns one.
        let _ = sqlx::query(
            "ALTER TABLE users ADD COLUMN phone TEXT",
        )
        .execute(&pool)
        .await; // Ignore error if column already exists

        let _ = sqlx::query(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_users_feishu_open_id ON users(feishu_open_id)",
        )
        .execute(&pool)
        .await; // Ignore error if index already exists

        // A unique index treats NULLs as distinct, so legacy rows with no phone
        // coexist fine while bound numbers stay one-per-account.
        let _ = sqlx::query(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_users_phone ON users(phone)",
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
            phone: None,
            created_at: chrono::Utc::now().naive_utc().to_string(),
            is_public,
            is_admin: false,
            deleted_at: None,
        })
    }

    /// Look up a user by username.
    pub async fn get_user_by_username(&self, username: &str) -> Result<Option<User>> {
        let user = sqlx::query_as::<_, User>(
            "SELECT id, username, password_hash, feishu_open_id, phone, created_at, is_public, is_admin, deleted_at FROM users WHERE username = ?",
        )
        .bind(username)
        .fetch_optional(&self.pool)
        .await?;

        Ok(user)
    }

    /// Look up a user by ID.
    pub async fn get_user_by_id(&self, user_id: &str) -> Result<Option<User>> {
        let user = sqlx::query_as::<_, User>(
            "SELECT id, username, password_hash, feishu_open_id, phone, created_at, is_public, is_admin, deleted_at FROM users WHERE id = ?",
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
            "SELECT id, username, password_hash, feishu_open_id, phone, created_at, is_public, is_admin, deleted_at FROM users WHERE is_public = 1 AND deleted_at IS NULL ORDER BY username",
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

    /// Rename an account. Fails if the new name is already taken.
    pub async fn update_username(&self, user_id: &str, username: &str) -> Result<()> {
        let result = sqlx::query("UPDATE users SET username = ? WHERE id = ?")
            .bind(username)
            .bind(user_id)
            .execute(&self.pool)
            .await;

        match result {
            Ok(r) if r.rows_affected() == 0 => anyhow::bail!("User not found"),
            Ok(_) => Ok(()),
            Err(e) if e.to_string().contains("UNIQUE constraint") => {
                anyhow::bail!("Username '{}' already exists", username)
            }
            Err(e) => Err(anyhow::anyhow!("Failed to update username: {}", e)),
        }
    }

    /// Look up a user by Feishu open_id.
    pub async fn get_user_by_feishu_open_id(&self, open_id: &str) -> Result<Option<User>> {
        let user = sqlx::query_as::<_, User>(
            "SELECT id, username, password_hash, feishu_open_id, phone, created_at, is_public, is_admin, deleted_at FROM users WHERE feishu_open_id = ?",
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
            phone: None,
            created_at: chrono::Utc::now().naive_utc().to_string(),
            is_public: false,
            is_admin: false,
            deleted_at: None,
        })
    }

    // ── Phone numbers (SMS verification-code login) ──

    /// Look up a user by phone number.
    ///
    /// Deliberately does **not** filter on `deleted_at`: a trashed account still
    /// occupies its phone number in the unique index, so filtering here would
    /// make a re-login attempt fall through to INSERT and fail with a 500.
    /// Callers must inspect `deleted_at` themselves and report the account as
    /// deleted.
    pub async fn get_user_by_phone(&self, phone: &str) -> Result<Option<User>> {
        let user = sqlx::query_as::<_, User>(
            "SELECT id, username, password_hash, feishu_open_id, phone, created_at, is_public, is_admin, deleted_at FROM users WHERE phone = ?",
        )
        .bind(phone)
        .fetch_optional(&self.pool)
        .await?;

        Ok(user)
    }

    /// Get or create the account owning `phone`.
    ///
    /// SMS signup collects no username, so a display name is derived from the
    /// last four digits. The password is a sentinel that Argon2 cannot parse,
    /// making password login fail closed for these accounts.
    pub async fn get_or_create_by_phone(&self, phone: &str) -> Result<User> {
        if let Some(user) = self.get_user_by_phone(phone).await? {
            return Ok(user);
        }

        let id = uuid::Uuid::new_v4().to_string();
        let password_hash = sms_only_password_hash();
        let created_at = chrono::Utc::now().naive_utc().to_string();
        let base = default_username(phone);

        for attempt in 0..5 {
            let username = if attempt == 0 {
                base.clone()
            } else {
                format!("{}{}", base, random_suffix())
            };

            let result = sqlx::query(
                "INSERT INTO users (id, username, password_hash, phone) VALUES (?, ?, ?, ?)",
            )
            .bind(&id)
            .bind(&username)
            .bind(&password_hash)
            .bind(phone)
            .execute(&self.pool)
            .await;

            match result {
                Ok(_) => {
                    return Ok(User {
                        id,
                        username,
                        password_hash,
                        feishu_open_id: None,
                        phone: Some(phone.to_string()),
                        created_at,
                        is_public: false,
                        is_admin: false,
                        deleted_at: None,
                    });
                }
                Err(e) if e.to_string().contains("UNIQUE constraint") => {
                    // A concurrent login may have created this phone first —
                    // that is a success, not a collision to retry.
                    if let Some(user) = self.get_user_by_phone(phone).await? {
                        return Ok(user);
                    }
                    // Otherwise the display name is taken; loop for a new suffix.
                }
                Err(e) => return Err(anyhow::anyhow!("Failed to create user: {}", e)),
            }
        }

        anyhow::bail!("Failed to allocate a username for phone number")
    }

    /// Bind or rebind `phone` to an account. Fails if another account already
    /// holds the number.
    pub async fn set_user_phone(&self, user_id: &str, phone: &str) -> Result<()> {
        let result = sqlx::query("UPDATE users SET phone = ? WHERE id = ?")
            .bind(phone)
            .bind(user_id)
            .execute(&self.pool)
            .await;

        match result {
            Ok(r) if r.rows_affected() == 0 => anyhow::bail!("User not found"),
            Ok(_) => Ok(()),
            Err(e) if e.to_string().contains("UNIQUE constraint") => anyhow::bail!(
                "Phone number is already bound to another account"
            ),
            Err(e) => Err(anyhow::anyhow!("Failed to bind phone number: {}", e)),
        }
    }

    /// Remove the phone binding from an account (admin recovery action).
    pub async fn clear_user_phone(&self, user_id: &str) -> Result<()> {
        let result = sqlx::query("UPDATE users SET phone = NULL WHERE id = ?")
            .bind(user_id)
            .execute(&self.pool)
            .await?;

        if result.rows_affected() == 0 {
            anyhow::bail!("User not found");
        }
        Ok(())
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
    /// = all users. `query` filters by username or phone substring
    /// (case-insensitive). Callers must only expose safe fields (never
    /// `password_hash`).
    pub async fn list_users(&self, query: Option<&str>, status: &str) -> Result<Vec<User>> {
        let status_clause = match status {
            "active" => " AND deleted_at IS NULL",
            "trash" => " AND deleted_at IS NOT NULL",
            _ => "",
        };
        let search = query.map(|q| format!("%{}%", q.trim().to_lowercase()));
        let sql = format!(
            "SELECT id, username, password_hash, feishu_open_id, phone, created_at, is_public, is_admin, deleted_at \
             FROM users WHERE 1=1{}{} ORDER BY created_at DESC",
            status_clause,
            if search.is_some() {
                " AND (LOWER(username) LIKE ? OR phone LIKE ?)"
            } else {
                ""
            },
        );

        let mut q = sqlx::query_as::<_, User>(&sql);
        if let Some(s) = &search {
            q = q.bind(s).bind(s);
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

    /// Set the `is_public` flag for a user (admin action).
    /// Fails if the user does not exist.
    pub async fn set_user_public(&self, user_id: &str, is_public: bool) -> Result<()> {
        let result = sqlx::query("UPDATE users SET is_public = ? WHERE id = ?")
            .bind(is_public)
            .bind(user_id)
            .execute(&self.pool)
            .await?;

        if result.rows_affected() == 0 {
            anyhow::bail!("User not found");
        }
        Ok(())
    }
}

/// Password hash for an account that may only authenticate by SMS.
///
/// The `!sms-auth-only!:` prefix is not a valid Argon2 encoding, so
/// `PasswordHash::new` rejects it and password verification fails closed rather
/// than silently matching. The random tail keeps every sentinel distinct.
pub fn sms_only_password_hash() -> String {
    format!("!sms-auth-only!:{}", uuid::Uuid::new_v4())
}

/// Display name for an account provisioned by SMS signup, which collects none.
///
/// Only the last four digits are used. A full number would publish it to every
/// other user through `list_public_users` and into LLM prompts; a masked form
/// like `138****8000` still reveals 7 of 11 digits, leaving only 10^4
/// candidates to brute-force the rest. "尾号 8000" is the convention banks and
/// carriers already use, and it keeps `@`-mentions typeable.
fn default_username(phone: &str) -> String {
    let tail = &phone[phone.len().saturating_sub(4)..];
    format!("用户{}", tail)
}

/// Short suffix used when the derived display name is already taken.
/// Ambiguous characters (0, 1, i, l, o) are excluded so the name stays legible.
fn random_suffix() -> String {
    const ALPHABET: &[u8] = b"23456789abcdefghjkmnpqrstuvwxyz";
    uuid::Uuid::new_v4()
        .as_bytes()
        .iter()
        .take(4)
        .map(|b| ALPHABET[(*b % ALPHABET.len() as u8) as usize] as char)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The TempDir must be returned and held by the test — dropping it deletes
    /// the database file underneath the pool.
    async fn setup() -> (tempfile::TempDir, UserStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = UserStore::new(dir.path().to_str().unwrap()).await.unwrap();
        (dir, store)
    }

    #[test]
    fn default_username_shows_only_the_last_four_digits() {
        assert_eq!(default_username("13800138000"), "用户8000");
        assert_eq!(default_username("123"), "用户123");
        assert_eq!(default_username(""), "用户");
    }

    #[test]
    fn random_suffix_is_four_legible_characters() {
        let s = random_suffix();
        assert_eq!(s.chars().count(), 4);
        assert!(s.chars().all(|c| "23456789abcdefghjkmnpqrstuvwxyz".contains(c)));
        assert_ne!(random_suffix(), random_suffix());
    }

    #[tokio::test]
    async fn get_or_create_by_phone_is_idempotent() {
        let (_dir, store) = setup().await;
        let first = store.get_or_create_by_phone("13800001111").await.unwrap();
        let second = store.get_or_create_by_phone("13800001111").await.unwrap();

        assert_eq!(first.id, second.id);
        assert_eq!(first.username, "用户1111");
        assert_eq!(second.phone.as_deref(), Some("13800001111"));
        assert!(!second.is_public, "SMS-provisioned accounts start private");
        assert!(
            second.password_hash.starts_with("!sms-auth-only!:"),
            "password must be an unusable sentinel"
        );
    }

    #[tokio::test]
    async fn colliding_display_names_get_distinct_usernames() {
        let (_dir, store) = setup().await;
        let a = store.get_or_create_by_phone("13800001111").await.unwrap();
        let b = store.get_or_create_by_phone("13900001111").await.unwrap();

        assert_eq!(a.username, "用户1111");
        assert_ne!(a.id, b.id);
        assert!(
            b.username.starts_with("用户1111"),
            "suffixed name keeps the recognizable prefix, got {}",
            b.username
        );
        assert!(b.username.len() > a.username.len());
    }

    #[tokio::test]
    async fn set_user_phone_rejects_a_number_held_by_another_account() {
        let (_dir, store) = setup().await;
        let owner = store.get_or_create_by_phone("13800002222").await.unwrap();
        let other = store
            .create_user("legacy", "somehash", false)
            .await
            .unwrap();

        assert!(store.set_user_phone(&other.id, "13800002222").await.is_err());

        let err = store
            .set_user_phone(&other.id, "13800002222")
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("already bound"), "got: {}", err);

        // A free number binds fine, and the owner keeps theirs.
        store.set_user_phone(&other.id, "13800003333").await.unwrap();
        assert_eq!(
            store.get_user_by_id(&other.id).await.unwrap().unwrap().phone.as_deref(),
            Some("13800003333")
        );
        assert_eq!(
            store.get_user_by_phone("13800002222").await.unwrap().unwrap().id,
            owner.id
        );
    }

    #[tokio::test]
    async fn get_user_by_phone_still_finds_trashed_accounts() {
        let (_dir, store) = setup().await;
        let user = store.get_or_create_by_phone("13800004444").await.unwrap();

        store.soft_delete_user(&user.id).await.unwrap();

        // The caller decides how to report a deleted account; hiding it here
        // would send login down the INSERT path and hit the unique index.
        let found = store.get_user_by_phone("13800004444").await.unwrap();
        assert!(found.is_some(), "trashed account must still be findable");
        assert!(found.unwrap().deleted_at.is_some());

        store.clear_user_phone(&user.id).await.unwrap();
        assert!(store.get_user_by_phone("13800004444").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn set_and_clear_phone_report_missing_users() {
        let (_dir, store) = setup().await;
        assert!(store.set_user_phone("nope", "13800005555").await.is_err());
        assert!(store.clear_user_phone("nope").await.is_err());
    }
}
