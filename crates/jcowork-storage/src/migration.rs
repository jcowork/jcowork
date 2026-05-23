//! Database schema migrations.

use anyhow::Result;
use sqlx::SqlitePool;
use tracing::info;

/// Run all database migrations for a user's database.
pub async fn run_migrations(pool: &SqlitePool) -> Result<()> {
    // Users table (global metadata, also stored per-user for convenience)
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS users (
            id TEXT PRIMARY KEY,
            username TEXT NOT NULL UNIQUE,
            password_hash TEXT NOT NULL,
            api_key TEXT,
            preferences TEXT DEFAULT '{}',
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        )
        "#,
    )
    .execute(pool)
    .await?;

    // Sessions table
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS sessions (
            id TEXT PRIMARY KEY,
            user_id TEXT NOT NULL,
            title TEXT,
            model TEXT DEFAULT 'openai:gpt-4o',
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        )
        "#,
    )
    .execute(pool)
    .await?;

    // Messages table
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS messages (
            id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL,
            role TEXT NOT NULL CHECK(role IN ('system', 'user', 'assistant', 'tool')),
            content TEXT NOT NULL DEFAULT '',
            tool_calls TEXT,
            tool_call_id TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
        )
        "#,
    )
    .execute(pool)
    .await?;

    // Memories table with FTS5
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS memories (
            id TEXT PRIMARY KEY,
            user_id TEXT NOT NULL,
            content TEXT NOT NULL,
            category TEXT DEFAULT 'general',
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        )
        "#,
    )
    .execute(pool)
    .await?;

    // FTS5 virtual table for memory search
    sqlx::query(
        r#"
        CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts USING fts5(
            content,
            category,
            content='memories',
            content_rowid=rowid,
            tokenize='porter unicode61'
        )
        "#,
    )
    .execute(pool)
    .await?;

    // FTS5 sync triggers
    sqlx::query(
        r#"
        CREATE TRIGGER IF NOT EXISTS memories_fts_insert AFTER INSERT ON memories BEGIN
            INSERT INTO memories_fts(rowid, content, category)
            VALUES (new.rowid, new.content, new.category);
        END
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TRIGGER IF NOT EXISTS memories_fts_delete AFTER DELETE ON memories BEGIN
            INSERT INTO memories_fts(memories_fts, rowid, content, category)
            VALUES ('delete', old.rowid, old.content, old.category);
        END
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TRIGGER IF NOT EXISTS memories_fts_update AFTER UPDATE ON memories BEGIN
            INSERT INTO memories_fts(memories_fts, rowid, content, category)
            VALUES ('delete', old.rowid, old.content, old.category);
            INSERT INTO memories_fts(rowid, content, category)
            VALUES (new.rowid, new.content, new.category);
        END
        "#,
    )
    .execute(pool)
    .await?;

    // Skills table
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS skills (
            id TEXT PRIMARY KEY,
            user_id TEXT NOT NULL,
            name TEXT NOT NULL,
            description TEXT,
            content TEXT NOT NULL,
            conditions TEXT,
            version INTEGER DEFAULT 1,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now')),
            UNIQUE(user_id, name)
        )
        "#,
    )
    .execute(pool)
    .await?;

    // Cron jobs table
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS cron_jobs (
            id TEXT PRIMARY KEY,
            user_id TEXT NOT NULL,
            schedule TEXT NOT NULL,
            prompt TEXT NOT NULL,
            platform TEXT DEFAULT 'api',
            enabled INTEGER DEFAULT 1,
            last_run TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        )
        "#,
    )
    .execute(pool)
    .await?;

    // Todos table
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS todos (
            id TEXT PRIMARY KEY,
            user_id TEXT NOT NULL,
            content TEXT NOT NULL,
            completed INTEGER DEFAULT 0,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        )
        "#,
    )
    .execute(pool)
    .await?;

    // Indexes
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id)")
        .execute(pool)
        .await?;

    sqlx::query("CREATE INDEX IF NOT EXISTS idx_memories_user ON memories(user_id)")
        .execute(pool)
        .await?;

    sqlx::query("CREATE INDEX IF NOT EXISTS idx_skills_user ON skills(user_id)")
        .execute(pool)
        .await?;

    sqlx::query("CREATE INDEX IF NOT EXISTS idx_cron_jobs_user ON cron_jobs(user_id)")
        .execute(pool)
        .await?;

    sqlx::query("CREATE INDEX IF NOT EXISTS idx_todos_user ON todos(user_id)")
        .execute(pool)
        .await?;

    info!("Database migrations completed");
    Ok(())
}
