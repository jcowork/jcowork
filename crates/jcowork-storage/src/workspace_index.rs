//! Workspace document indexing with full-text search.
//!
//! Provides per-user SQLite-based document indexing for files uploaded to the workspace.
//! PDF files are automatically parsed via pdftext before indexing.
//! Text-based files (md, html, etc.) are indexed directly from their content.

use anyhow::Result;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions, SqliteJournalMode};
use sqlx::SqlitePool;
use std::path::Path;
use std::str::FromStr;
use tokio::process::Command;
use tracing::{info, warn};

/// Resolve the Python binary path in the jcowork venv.
fn resolve_python_bin() -> String {
    let base = shellexpand::tilde("~/.jcowork/venv").to_string();
    if cfg!(windows) {
        format!("{}\\Scripts\\python.exe", base)
    } else {
        format!("{}/bin/python", base)
    }
}

/// Python script for PDF text extraction using pdftext.
const PDF_EXTRACT_SCRIPT: &str = r#"
import sys
from pdftext.extraction import plain_text_output

path = sys.argv[1]
try:
    text = plain_text_output(path)
    print(text, end='')
except Exception as e:
    print(f"ERROR: {e}", file=sys.stderr)
    sys.exit(1)
"#;

/// Maximum content size to index (characters). Larger content is truncated.
const MAX_INDEX_CONTENT_LEN: usize = 50_000;

/// Manages per-user workspace document index.
///
/// Each user gets their own SQLite database with FTS5 full-text search
/// for quickly finding documents by content or filename.
#[derive(Debug, Clone)]
pub struct WorkspaceIndex {
    pool: SqlitePool,
}

impl WorkspaceIndex {
    /// Create or open the workspace index database for a user.
    pub async fn new(data_dir: &str, user_id: &str) -> Result<Self> {
        let user_dir = format!("{}/{}", data_dir, user_id);
        tokio::fs::create_dir_all(&user_dir).await?;

        let db_path = format!("{}/workspace_index.db", user_dir);
        let db_url = format!("sqlite:{}?mode=rwc", db_path);

        let options = SqliteConnectOptions::from_str(&db_url)?
            .journal_mode(SqliteJournalMode::Wal)
            .busy_timeout(std::time::Duration::from_secs(5));

        let pool = SqlitePoolOptions::new()
            .max_connections(3)
            .connect_with(options)
            .await?;

        Self::run_migrations(&pool).await?;
        info!(user_id = user_id, "Workspace index initialized");
        Ok(Self { pool })
    }

    /// Run database migrations to create required tables.
    async fn run_migrations(pool: &SqlitePool) -> Result<()> {
        // Main documents table — uses INTEGER PRIMARY KEY for FTS5 rowid compatibility
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS documents (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                file_path TEXT NOT NULL UNIQUE,
                dir_path TEXT NOT NULL,
                filename TEXT NOT NULL,
                content_type TEXT NOT NULL DEFAULT 'text',
                size INTEGER NOT NULL DEFAULT 0,
                content_text TEXT NOT NULL DEFAULT '',
                indexed_at TEXT NOT NULL DEFAULT (datetime('now'))
            )
            "#,
        )
        .execute(pool)
        .await?;

        // FTS5 virtual table for full-text search on filename + content
        sqlx::query(
            r#"
            CREATE VIRTUAL TABLE IF NOT EXISTS documents_fts USING fts5(
                filename,
                content_text,
                content='documents',
                content_rowid='id'
            )
            "#,
        )
        .execute(pool)
        .await?;

        // Triggers to keep FTS in sync with documents table
        sqlx::query(
            r#"
            CREATE TRIGGER IF NOT EXISTS documents_fts_insert AFTER INSERT ON documents BEGIN
                INSERT INTO documents_fts(rowid, filename, content_text)
                VALUES (NEW.id, NEW.filename, NEW.content_text);
            END
            "#,
        )
        .execute(pool)
        .await?;

        sqlx::query(
            r#"
            CREATE TRIGGER IF NOT EXISTS documents_fts_delete AFTER DELETE ON documents BEGIN
                INSERT INTO documents_fts(documents_fts, rowid, filename, content_text)
                VALUES ('delete', OLD.id, OLD.filename, OLD.content_text);
            END
            "#,
        )
        .execute(pool)
        .await?;

        sqlx::query(
            r#"
            CREATE TRIGGER IF NOT EXISTS documents_fts_update AFTER UPDATE ON documents BEGIN
                INSERT INTO documents_fts(documents_fts, rowid, filename, content_text)
                VALUES ('delete', OLD.id, OLD.filename, OLD.content_text);
                INSERT INTO documents_fts(rowid, filename, content_text)
                VALUES (NEW.id, NEW.filename, NEW.content_text);
            END
            "#,
        )
        .execute(pool)
        .await?;

        // Index on dir_path for directory-based queries
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_documents_dir ON documents(dir_path)")
            .execute(pool)
            .await?;

        Ok(())
    }

    /// Add or update a document in the index.
    ///
    /// For PDF files, automatically parses the content using pdftext.
    /// For text files, reads the content directly.
    /// Binary files (xlsx, docx, etc.) are indexed with metadata only.
    pub async fn add_document(&self, file_path: &str, workspace_root: &str) -> Result<()> {
        let full_path = Path::new(workspace_root).join(file_path);
        if !full_path.exists() {
            anyhow::bail!("File does not exist: {}", file_path);
        }

        let filename = full_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(file_path)
            .to_string();

        let dir_path = Path::new(file_path)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        let dir_path = if dir_path.is_empty() { ".".to_string() } else { dir_path };

        let ext = full_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        let content_type = match ext.as_str() {
            "pdf" => "pdf",
            "md" | "markdown" => "markdown",
            "html" | "htm" => "html",
            "xlsx" | "xls" => "excel",
            "docx" | "doc" => "word",
            _ => "text",
        };

        let metadata = tokio::fs::metadata(&full_path).await?;
        let size = metadata.len() as i64;

        // Extract text content based on file type
        let content_text = if ext == "pdf" {
            // Parse PDF using pdftext
            match self.extract_pdf_text(&full_path.to_string_lossy()).await {
                Ok(text) => text,
                Err(e) => {
                    warn!(file = %file_path, err = %e, "Failed to parse PDF for indexing");
                    format!("[PDF parse error: {}]", e)
                }
            }
        } else if matches!(ext.as_str(), "md" | "markdown" | "html" | "htm" | "txt" | "csv" | "json" | "xml" | "yaml" | "yml" | "toml" | "rs" | "py" | "js" | "ts" | "tsx" | "jsx" | "css" | "sh" | "bash") {
            // Read text files directly
            match tokio::fs::read_to_string(&full_path).await {
                Ok(text) => text,
                Err(e) => {
                    warn!(file = %file_path, err = %e, "Failed to read text file for indexing");
                    String::new()
                }
            }
        } else {
            // Binary files (xlsx, docx, etc.) - no content extraction for now
            String::new()
        };

        // Truncate very long content
        let content_text = if content_text.len() > MAX_INDEX_CONTENT_LEN {
            let mut truncated = content_text[..MAX_INDEX_CONTENT_LEN].to_string();
            truncated.push_str("\n\n[... content truncated for indexing ...]");
            truncated
        } else {
            content_text
        };

        // Upsert into database
        let now = chrono::Utc::now().to_rfc3339();

        sqlx::query(
            r#"
            INSERT INTO documents (file_path, dir_path, filename, content_type, size, content_text, indexed_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            ON CONFLICT(file_path) DO UPDATE SET
                dir_path = ?2,
                filename = ?3,
                content_type = ?4,
                size = ?5,
                content_text = ?6,
                indexed_at = ?7
            "#,
        )
        .bind(file_path)
        .bind(&dir_path)
        .bind(&filename)
        .bind(content_type)
        .bind(size)
        .bind(&content_text)
        .bind(&now)
        .execute(&self.pool)
        .await?;

        info!(file = %file_path, content_type = %content_type, "Document indexed");
        Ok(())
    }

    /// Remove a file from the index.
    pub async fn remove_file(&self, file_path: &str) -> Result<()> {
        sqlx::query("DELETE FROM documents WHERE file_path = ?")
            .bind(file_path)
            .execute(&self.pool)
            .await?;
        info!(file = %file_path, "Document removed from index");
        Ok(())
    }

    /// Remove all documents under a directory from the index.
    pub async fn remove_directory(&self, dir_path: &str) -> Result<()> {
        // Remove the directory itself and all files under it
        let pattern = if dir_path == "." {
            "%".to_string()
        } else {
            format!("{}%", dir_path)
        };

        sqlx::query("DELETE FROM documents WHERE dir_path = ? OR dir_path LIKE ?")
            .bind(dir_path)
            .bind(&pattern)
            .execute(&self.pool)
            .await?;

        info!(dir = %dir_path, "Directory removed from index");
        Ok(())
    }

    /// Update index when a file or directory is moved.
    ///
    /// Updates file_path and dir_path for affected documents.
    pub async fn move_path(&self, from: &str, to: &str) -> Result<()> {
        // Check if it's a file move (exact match) or directory move (prefix match)
        let exact = sqlx::query(
            "UPDATE documents SET file_path = ?1, dir_path = ?2, indexed_at = ?3 WHERE file_path = ?4"
        )
        .bind(to)
        .bind(
            Path::new(to)
                .parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default(),
        )
        .bind(chrono::Utc::now().to_rfc3339())
        .bind(from)
        .execute(&self.pool)
        .await?;

        if exact.rows_affected() == 0 {
            // Might be a directory move - update all files under the directory
            let from_prefix = format!("{}/", from);

            // Get all documents under the old directory
            let docs = sqlx::query_as::<_, (i64, String)>(
                "SELECT id, file_path FROM documents WHERE file_path LIKE ?1 OR dir_path = ?2 OR dir_path LIKE ?3"
            )
            .bind(format!("{}%", from_prefix))
            .bind(from)
            .bind(format!("{}/%", from))
            .fetch_all(&self.pool)
            .await?;

            let count = docs.len();
            for (id, old_path) in docs {
                let new_path = old_path.replacen(from, to, 1);
                let new_dir = Path::new(&new_path)
                    .parent()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default();

                sqlx::query("UPDATE documents SET file_path = ?1, dir_path = ?2, indexed_at = ?3 WHERE id = ?4")
                    .bind(&new_path)
                    .bind(&new_dir)
                    .bind(chrono::Utc::now().to_rfc3339())
                    .bind(&id)
                    .execute(&self.pool)
                    .await?;
            }

            if count > 0 {
                info!(from = %from, to = %to, count = count, "Directory moved in index");
            }
        } else {
            info!(from = %from, to = %to, "File moved in index");
        }

        Ok(())
    }

    /// Search documents by keyword using FTS5.
    ///
    /// Returns matching documents with relevance scores and content snippets.
    pub async fn search(&self, query: &str, limit: u32) -> Result<Vec<IndexedDocument>> {
        // Use FTS5 search with snippet extraction
        let docs = sqlx::query_as::<_, IndexedDocumentRow>(
            r#"
            SELECT d.id, d.file_path, d.dir_path, d.filename, d.content_type, d.size, d.indexed_at,
                   snippet(documents_fts, 1, '>>>', '<<<', '...', 32) as snippet,
                   rank
            FROM documents_fts
            JOIN documents d ON d.id = documents_fts.rowid
            WHERE documents_fts MATCH ?1
            ORDER BY rank
            LIMIT ?2
            "#,
        )
        .bind(query)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;

        Ok(docs
            .into_iter()
            .map(|r| IndexedDocument {
                id: r.id,
                file_path: r.file_path,
                dir_path: r.dir_path,
                filename: r.filename,
                content_type: r.content_type,
                size: r.size,
                indexed_at: r.indexed_at,
                snippet: r.snippet,
            })
            .collect())
    }

    /// List all indexed documents in a directory.
    pub async fn list_by_directory(&self, dir_path: &str) -> Result<Vec<IndexedDocument>> {
        let docs = sqlx::query_as::<_, IndexedDocumentRow>(
            r#"
            SELECT id, file_path, dir_path, filename, content_type, size, indexed_at,
                   substr(content_text, 1, 200) as snippet
            FROM documents
            WHERE dir_path = ?1
            ORDER BY filename
            "#,
        )
        .bind(dir_path)
        .fetch_all(&self.pool)
        .await?;

        Ok(docs
            .into_iter()
            .map(|r| IndexedDocument {
                id: r.id,
                file_path: r.file_path,
                dir_path: r.dir_path,
                filename: r.filename,
                content_type: r.content_type,
                size: r.size,
                indexed_at: r.indexed_at,
                snippet: r.snippet,
            })
            .collect())
    }

    /// Get the full indexed content of a document by its file path.
    /// Returns None if the file is not indexed.
    pub async fn get_content(&self, file_path: &str) -> Result<Option<String>> {
        let row = sqlx::query_as::<_, (String,)>(
            "SELECT content_text FROM documents WHERE file_path = ?1"
        )
        .bind(file_path)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| r.0))
    }

    /// List all indexed documents (optionally filtered by directory prefix).
    pub async fn list_all(&self, dir_prefix: Option<&str>) -> Result<Vec<IndexedDocument>> {
        let docs = if let Some(prefix) = dir_prefix {
            let pattern = format!("{}%", prefix);
            sqlx::query_as::<_, IndexedDocumentRow>(
                r#"
                SELECT id, file_path, dir_path, filename, content_type, size, indexed_at,
                       substr(content_text, 1, 200) as snippet
                FROM documents
                WHERE dir_path LIKE ?1 OR dir_path = ?1
                ORDER BY dir_path, filename
                "#,
            )
            .bind(&pattern)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query_as::<_, IndexedDocumentRow>(
                r#"
                SELECT id, file_path, dir_path, filename, content_type, size, indexed_at,
                       substr(content_text, 1, 200) as snippet
                FROM documents
                ORDER BY dir_path, filename
                "#,
            )
            .fetch_all(&self.pool)
            .await?
        };

        Ok(docs
            .into_iter()
            .map(|r| IndexedDocument {
                id: r.id,
                file_path: r.file_path,
                dir_path: r.dir_path,
                filename: r.filename,
                content_type: r.content_type,
                size: r.size,
                indexed_at: r.indexed_at,
                snippet: r.snippet,
            })
            .collect())
    }

    /// Get the count of indexed documents.
    pub async fn count(&self) -> Result<i64> {
        let row = sqlx::query_as::<_, (i64,)>("SELECT COUNT(*) FROM documents")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.0)
    }

    /// Extract text from a PDF file using pdftext (Python).
    async fn extract_pdf_text(&self, pdf_path: &str) -> Result<String> {
        let python_bin = resolve_python_bin();

        if !Path::new(&python_bin).exists() {
            anyhow::bail!("Python venv not found. Run scripts/setup-python.sh first.");
        }

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(120),
            Command::new(&python_bin)
                .arg("-c")
                .arg(PDF_EXTRACT_SCRIPT)
                .arg(pdf_path)
                .output(),
        )
        .await;

        let output = match result {
            Ok(Ok(o)) => o,
            Ok(Err(e)) => anyhow::bail!("Failed to spawn pdftext: {}", e),
            Err(_) => anyhow::bail!("pdftext timed out after 120s for: {}", pdf_path),
        };

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("pdftext failed for {}: {}", pdf_path, stderr);
        }

        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        Ok(text)
    }
}

/// A document entry from the workspace index.
#[derive(Debug, Clone, serde::Serialize)]
pub struct IndexedDocument {
    pub id: i64,
    pub file_path: String,
    pub dir_path: String,
    pub filename: String,
    pub content_type: String,
    pub size: i64,
    pub indexed_at: String,
    pub snippet: String,
}

/// Internal row type for SQL query results.
#[derive(Debug, sqlx::FromRow)]
struct IndexedDocumentRow {
    id: i64,
    file_path: String,
    dir_path: String,
    filename: String,
    content_type: String,
    size: i64,
    indexed_at: String,
    snippet: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    async fn setup_test_index() -> (WorkspaceIndex, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let index = WorkspaceIndex::new(dir.path().to_str().unwrap(), "test-user")
            .await
            .unwrap();
        (index, dir)
    }

    #[tokio::test]
    async fn test_index_initialization() {
        let (index, _dir) = setup_test_index().await;
        assert_eq!(index.count().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn test_add_text_document() {
        let (index, dir) = setup_test_index().await;

        // Create a test file
        let workspace = dir.path().join("test-workspace");
        tokio::fs::create_dir_all(&workspace).await.unwrap();
        tokio::fs::write(workspace.join("test.md"), "# Hello World\nThis is a test document.")
            .await
            .unwrap();

        let ws_str = workspace.to_string_lossy().to_string();
        index.add_document("test.md", &ws_str).await.unwrap();

        assert_eq!(index.count().await.unwrap(), 1);

        let docs = index.list_by_directory(".").await.unwrap();
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].filename, "test.md");
        assert_eq!(docs[0].content_type, "markdown");
    }

    #[tokio::test]
    async fn test_search_documents() {
        let (index, dir) = setup_test_index().await;

        let workspace = dir.path().join("test-workspace");
        tokio::fs::create_dir_all(&workspace).await.unwrap();
        tokio::fs::write(
            workspace.join("report.html"),
            "<html><body>Financial quarterly report analysis</body></html>",
        )
        .await
        .unwrap();

        let ws_str = workspace.to_string_lossy().to_string();
        index.add_document("report.html", &ws_str).await.unwrap();

        // Search should find the document
        let results = index.search("financial", 10).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].filename, "report.html");
    }

    #[tokio::test]
    async fn test_remove_document() {
        let (index, dir) = setup_test_index().await;

        let workspace = dir.path().join("test-workspace");
        tokio::fs::create_dir_all(&workspace).await.unwrap();
        tokio::fs::write(workspace.join("temp.txt"), "temporary content").await.unwrap();

        let ws_str = workspace.to_string_lossy().to_string();
        index.add_document("temp.txt", &ws_str).await.unwrap();
        assert_eq!(index.count().await.unwrap(), 1);

        index.remove_file("temp.txt").await.unwrap();
        assert_eq!(index.count().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn test_move_document() {
        let (index, dir) = setup_test_index().await;

        let workspace = dir.path().join("test-workspace");
        tokio::fs::create_dir_all(workspace.join("old_dir")).await.unwrap();
        tokio::fs::write(workspace.join("old_dir/file.txt"), "content").await.unwrap();

        let ws_str = workspace.to_string_lossy().to_string();
        index.add_document("old_dir/file.txt", &ws_str).await.unwrap();

        // Move the file
        index.move_path("old_dir/file.txt", "new_dir/file.txt").await.unwrap();

        let docs = index.list_all(None).await.unwrap();
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].file_path, "new_dir/file.txt");
        assert_eq!(docs[0].dir_path, "new_dir");
    }

    #[tokio::test]
    async fn test_remove_directory() {
        let (index, dir) = setup_test_index().await;

        let workspace = dir.path().join("test-workspace");
        tokio::fs::create_dir_all(workspace.join("docs")).await.unwrap();
        tokio::fs::write(workspace.join("docs/a.txt"), "a").await.unwrap();
        tokio::fs::write(workspace.join("docs/b.txt"), "b").await.unwrap();

        let ws_str = workspace.to_string_lossy().to_string();
        index.add_document("docs/a.txt", &ws_str).await.unwrap();
        index.add_document("docs/b.txt", &ws_str).await.unwrap();
        assert_eq!(index.count().await.unwrap(), 2);

        index.remove_directory("docs").await.unwrap();
        assert_eq!(index.count().await.unwrap(), 0);
    }
}
