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

    // Memories table
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS memories (
            id TEXT PRIMARY KEY,
            user_id TEXT NOT NULL,
            content TEXT NOT NULL,
            content_tokens TEXT NOT NULL DEFAULT '',
            category TEXT DEFAULT 'general',
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        )
        "#,
    )
    .execute(pool)
    .await?;

    // NOTE: memories_fts (FTS5 virtual table) is managed exclusively by BuiltinMemoryProvider.
    // Do NOT create or alter memories_fts here to avoid schema conflicts.

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
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            name TEXT,
            model TEXT
        )
        "#,
    )
    .execute(pool)
    .await?;

    // Upgrade path for pre-existing databases missing the newer columns
    for col in ["name TEXT", "model TEXT"] {
        if let Err(e) = sqlx::query(&format!("ALTER TABLE cron_jobs ADD COLUMN {}", col))
            .execute(pool)
            .await
        {
            // Column already exists — safe to ignore
            tracing::debug!(error = %e, column = %col, "cron_jobs column migration skipped");
        }
    }

    // Cron task execution results table
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS cron_task_results (
            id TEXT PRIMARY KEY,
            cron_job_id TEXT NOT NULL,
            user_id TEXT NOT NULL,
            output TEXT NOT NULL DEFAULT '',
            status TEXT NOT NULL DEFAULT 'success',
            executed_at TEXT NOT NULL
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

    sqlx::query("CREATE INDEX IF NOT EXISTS idx_cron_task_results_job ON cron_task_results(cron_job_id)")
        .execute(pool)
        .await?;

    sqlx::query("CREATE INDEX IF NOT EXISTS idx_todos_user ON todos(user_id)")
        .execute(pool)
        .await?;

    // Feishu configs table (per-user Feishu app configuration)
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS feishu_configs (
            user_id TEXT NOT NULL PRIMARY KEY,
            app_id TEXT NOT NULL UNIQUE,
            app_secret TEXT NOT NULL,
            verification_token TEXT NOT NULL DEFAULT '',
            encrypt_key TEXT NOT NULL DEFAULT '',
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query("CREATE UNIQUE INDEX IF NOT EXISTS idx_feishu_configs_app_id ON feishu_configs(app_id)")
        .execute(pool)
        .await?;

    info!("Database migrations completed");
    Ok(())
}
