//! Database layer for the report search index.
//!
//! Manages two tables:
//!   documents — one row per indexed PDF (with dedup via SHA-256 hash)
//!   chunks    — text segments extracted from each document
//! Plus an FTS5 virtual table for full-text search.

use anyhow::Result;
use sqlx::SqlitePool;
use tracing::info;

/// Initialize SQLite connection pool and run migrations.
pub async fn init_pool(db_path: &str) -> Result<SqlitePool> {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(5)
        .connect(&format!("sqlite:{}?mode=rwc", db_path))
        .await?;
    run_migrations(&pool).await?;
    Ok(pool)
}

async fn run_migrations(pool: &SqlitePool) -> Result<()> {
    // Enable WAL mode for better concurrent read/write
    sqlx::query("PRAGMA journal_mode=WAL").execute(pool).await?;
    sqlx::query("PRAGMA foreign_keys=ON").execute(pool).await?;

    // Documents table — one row per parsed PDF
    sqlx::query(r#"
        CREATE TABLE IF NOT EXISTS documents (
            id          TEXT PRIMARY KEY,
            company     TEXT NOT NULL,
            filename    TEXT NOT NULL,
            file_path   TEXT NOT NULL UNIQUE,
            doc_type    TEXT NOT NULL DEFAULT 'other',
            year        INTEGER,
            file_hash   TEXT NOT NULL,
            parsed_at   TEXT NOT NULL,
            total_chunks INTEGER NOT NULL DEFAULT 0
        )
    "#).execute(pool).await?;

    // Chunks table — text segments from each document
    sqlx::query(r#"
        CREATE TABLE IF NOT EXISTS chunks (
            id          TEXT PRIMARY KEY,
            doc_id      TEXT NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
            chunk_index INTEGER NOT NULL,
            content     TEXT NOT NULL
        )
    "#).execute(pool).await?;

    // FTS5 virtual table for full-text search
    // unicode61 tokenizer handles CJK by treating each character as a token
    sqlx::query(r#"
        CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts USING fts5(
            content,
            content='chunks',
            content_rowid='rowid',
            tokenize='unicode61'
        )
    "#).execute(pool).await?;

    // Sync triggers: keep chunks_fts in sync with chunks table
    sqlx::query(r#"
        CREATE TRIGGER IF NOT EXISTS chunks_fts_insert
        AFTER INSERT ON chunks BEGIN
            INSERT INTO chunks_fts(rowid, content) VALUES (new.rowid, new.content);
        END
    "#).execute(pool).await?;

    sqlx::query(r#"
        CREATE TRIGGER IF NOT EXISTS chunks_fts_delete
        AFTER DELETE ON chunks BEGIN
            INSERT INTO chunks_fts(chunks_fts, rowid, content) VALUES ('delete', old.rowid, old.content);
        END
    "#).execute(pool).await?;

    sqlx::query(r#"
        CREATE TRIGGER IF NOT EXISTS chunks_fts_update
        AFTER UPDATE ON chunks BEGIN
            INSERT INTO chunks_fts(chunks_fts, rowid, content) VALUES ('delete', old.rowid, old.content);
            INSERT INTO chunks_fts(rowid, content) VALUES (new.rowid, new.content);
        END
    "#).execute(pool).await?;

    // Indexes
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_chunks_doc_id ON chunks(doc_id)")
        .execute(pool).await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_documents_company ON documents(company)")
        .execute(pool).await?;

    info!("Report search DB migrations completed");
    Ok(())
}
