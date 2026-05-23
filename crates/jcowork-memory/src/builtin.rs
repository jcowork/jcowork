//! Built-in SQLite FTS5 memory provider with jieba Chinese tokenization.

use anyhow::Result;
use async_trait::async_trait;
use jieba_rs::Jieba;
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::models::{MemoryEntry, MemorySearchResult};
use crate::provider::MemoryProvider;

/// Tokenize text using jieba for Chinese segmentation.
/// Returns a space-separated string of tokens suitable for FTS5.
fn tokenize_for_fts(text: &str) -> String {
    use std::sync::OnceLock;
    static JIEBA: OnceLock<Jieba> = OnceLock::new();
    let jieba = JIEBA.get_or_init(Jieba::new);
    let tokens = jieba.cut(text, false); // cut for search (not HMM)
    tokens.join(" ")
}

/// Built-in memory provider using SQLite FTS5 for full-text search.
///
/// Each user has their own SQLite database, so user isolation is handled at the database layer.
pub struct BuiltinMemoryProvider {
    pool: SqlitePool,
}

impl BuiltinMemoryProvider {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Initialize the database schema (create tables if not exist).
    pub async fn init(&self) -> Result<()> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS memories (
                id TEXT PRIMARY KEY,
                user_id TEXT NOT NULL,
                content TEXT NOT NULL,
                content_tokens TEXT NOT NULL DEFAULT '',
                category TEXT NOT NULL DEFAULT 'general',
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            "#,
        )
        .execute(&self.pool)
        .await?;

        // Remove any stale FTS triggers created by old migrations (they reference wrong columns)
        for trigger in ["memories_fts_insert", "memories_fts_delete", "memories_fts_update"] {
            sqlx::query(&format!("DROP TRIGGER IF EXISTS {}", trigger))
                .execute(&self.pool)
                .await?;
        }

        // Drop old FTS5 table if it exists, recreate in standalone mode
        // (no content= sync — we manage FTS rows manually with jieba-tokenized content)
        sqlx::query("DROP TABLE IF EXISTS memories_fts")
            .execute(&self.pool)
            .await?;

        // Create FTS5 virtual table in standalone mode
        sqlx::query(
            r#"
            CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts
            USING fts5(content_tokens);
            "#,
        )
        .execute(&self.pool)
        .await?;

        // Create index on user_id
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_memories_user_id ON memories(user_id)")
            .execute(&self.pool)
            .await?;

        // Rebuild FTS index from existing data (tokenized content)
        sqlx::query(
            r#"INSERT INTO memories_fts(rowid, content_tokens)
               SELECT rowid, content_tokens FROM memories WHERE content_tokens != ''"#,
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }
}

#[async_trait]
impl MemoryProvider for BuiltinMemoryProvider {
    fn name(&self) -> &str {
        "builtin"
    }

    async fn save(&self, user_id: &str, content: &str, category: &str) -> Result<MemoryEntry> {
        let id = Uuid::new_v4().to_string();
        let now = chrono::Utc::now().naive_utc().to_string();
        let content_tokens = tokenize_for_fts(content);

        // Insert into memories table
        sqlx::query(
            r#"INSERT INTO memories (id, user_id, content, content_tokens, category, created_at, updated_at)
               VALUES (?, ?, ?, ?, ?, ?, ?)"#,
        )
        .bind(&id)
        .bind(user_id)
        .bind(content)
        .bind(&content_tokens)
        .bind(category)
        .bind(&now)
        .bind(&now)
        .execute(&self.pool)
        .await?;

        // Insert into FTS5 index (standalone mode — no auto-sync)
        sqlx::query(
            r#"INSERT INTO memories_fts(rowid, content_tokens)
               SELECT rowid, content_tokens FROM memories WHERE id = ?"#,
        )
        .bind(&id)
        .execute(&self.pool)
        .await?;

        Ok(MemoryEntry {
            id,
            user_id: user_id.to_string(),
            content: content.to_string(),
            category: category.to_string(),
            created_at: now.clone(),
            updated_at: now,
        })
    }

    async fn recall_all(&self, user_id: &str) -> Result<Vec<MemoryEntry>> {
        let rows = sqlx::query_as::<_, (String, String, String, String, String, String)>(
            r#"SELECT id, user_id, content, category, created_at, updated_at
               FROM memories WHERE user_id = ? ORDER BY created_at DESC"#,
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|(id, user_id, content, category, created_at, updated_at)| MemoryEntry {
                id,
                user_id,
                content,
                category,
                created_at,
                updated_at,
            })
            .collect())
    }

    async fn search(&self, user_id: &str, query: &str, limit: usize) -> Result<Vec<MemorySearchResult>> {
        // Tokenize the search query with jieba for CJK-aware matching
        let tokenized_query = tokenize_for_fts(query);
        // Join tokens with OR for broader matching
        let fts_query = tokenized_query
            .split_whitespace()
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join(" OR ");
    
        if fts_query.is_empty() {
            return Ok(Vec::new());
        }
    
        let fts_rows = sqlx::query_as::<_, (String, String, String, f64)>(
            r#"SELECT m.id, m.content, m.category, f.rank
               FROM memories_fts f
               JOIN memories m ON m.rowid = f.rowid
               WHERE memories_fts MATCH ? AND m.user_id = ?
               ORDER BY f.rank
               LIMIT ?"#,
        )
        .bind(&fts_query)
        .bind(user_id)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .unwrap_or_default();
    
        if !fts_rows.is_empty() {
            return Ok(fts_rows
                .into_iter()
                .map(|(id, content, category, rank)| MemorySearchResult {
                    id,
                    content,
                    category,
                    rank,
                })
                .collect());
        }
    
        // Fallback: LIKE search (handles edge cases jieba might miss)
        let like_pattern = format!("%{}%", query.replace('%', "\\%").replace('_', "\\_"));
        let like_rows = sqlx::query_as::<_, (String, String, String)>(
            r#"SELECT id, content, category
               FROM memories
               WHERE user_id = ? AND content LIKE ? ESCAPE '\\'
               ORDER BY updated_at DESC
               LIMIT ?"#,
        )
        .bind(user_id)
        .bind(&like_pattern)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;
    
        Ok(like_rows
            .into_iter()
            .map(|(id, content, category)| MemorySearchResult {
                id,
                content,
                category,
                rank: 1.0,
            })
            .collect())
    }

    async fn delete(&self, user_id: &str, memory_id: &str) -> Result<()> {
        // Get rowid before deleting from memories (needed for FTS5)
        let rowid: Option<i64> = sqlx::query_scalar(
            "SELECT rowid FROM memories WHERE id = ? AND user_id = ?",
        )
        .bind(memory_id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?;

        if let Some(rid) = rowid {
            // Delete from FTS5 first
            sqlx::query("DELETE FROM memories_fts WHERE rowid = ?")
                .bind(rid)
                .execute(&self.pool)
                .await?;
        }

        // Delete from memories table
        sqlx::query("DELETE FROM memories WHERE id = ? AND user_id = ?")
            .bind(memory_id)
            .bind(user_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn update(&self, user_id: &str, memory_id: &str, content: Option<&str>, category: Option<&str>) -> Result<MemoryEntry> {
        // Fetch existing entry
        let existing: Option<(String, String, String, String, String, String)> = sqlx::query_as(
            r#"SELECT id, user_id, content, category, created_at, updated_at
               FROM memories WHERE id = ? AND user_id = ?"#,
        )
        .bind(memory_id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?;

        let (id, uid, old_content, old_category, created_at, _) =
            existing.ok_or_else(|| anyhow::anyhow!("Memory not found"))?;

        let new_content = content.unwrap_or(&old_content);
        let new_category = category.unwrap_or(&old_category);
        let now = chrono::Utc::now().naive_utc().to_string();
        let new_tokens = tokenize_for_fts(new_content);

        // Update memories table
        sqlx::query(
            r#"UPDATE memories SET content = ?, content_tokens = ?, category = ?, updated_at = ?
               WHERE id = ? AND user_id = ?"#,
        )
        .bind(new_content)
        .bind(&new_tokens)
        .bind(new_category)
        .bind(&now)
        .bind(memory_id)
        .bind(user_id)
        .execute(&self.pool)
        .await?;

        // Update FTS5 index: delete old, insert new
        let rowid: i64 = sqlx::query_scalar("SELECT rowid FROM memories WHERE id = ?")
            .bind(memory_id)
            .fetch_one(&self.pool)
            .await?;

        sqlx::query("DELETE FROM memories_fts WHERE rowid = ?")
            .bind(rowid)
            .execute(&self.pool)
            .await?;

        sqlx::query("INSERT INTO memories_fts(rowid, content_tokens) VALUES (?, ?)")
            .bind(rowid)
            .bind(&new_tokens)
            .execute(&self.pool)
            .await?;

        Ok(MemoryEntry {
            id,
            user_id: uid,
            content: new_content.to_string(),
            category: new_category.to_string(),
            created_at,
            updated_at: now,
        })
    }
}
