//! Workspace document indexing with full-text search and vector search.
//!
//! Provides per-user SQLite-based document indexing for files uploaded to the workspace.
//! PDF files are automatically parsed via Docling service into structured Markdown.
//! Text-based files (md, html, etc.) are indexed directly from their content.
//! Document chunks are embedded using the Docling embedding service for semantic search.

use anyhow::Result;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions, SqliteJournalMode};
use sqlx::SqlitePool;
use std::path::Path;
use std::str::FromStr;
use tracing::{info, warn};

use crate::doc_chunker::{DocChunk, chunk_markdown};
use crate::embedding_client::{EmbeddingClient, bytes_to_embedding, embedding_to_bytes, cosine_similarity};

/// Maximum content size to index (characters). Larger content is truncated.
const MAX_INDEX_CONTENT_LEN: usize = 50_000;

/// Manages per-user workspace document index.
///
/// Each user gets their own SQLite database with FTS5 full-text search
/// for quickly finding documents by content or filename.
#[derive(Debug, Clone)]
pub struct WorkspaceIndex {
    pool: SqlitePool,
    data_dir: String,
    user_id: String,
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
        Ok(Self {
            pool,
            data_dir: data_dir.to_string(),
            user_id: user_id.to_string(),
        })
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

        // Add doc_hash column if it doesn't exist (for Docling image assets)
        // SQLite doesn't support IF NOT EXISTS for ADD COLUMN, so we use a try-insert approach
        sqlx::query("ALTER TABLE documents ADD COLUMN doc_hash TEXT DEFAULT ''")
            .execute(pool)
            .await
            .ok(); // Ignore error if column already exists

        // --- Vector search tables ---
        
        // Document chunks table (for semantic search)
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS doc_chunks (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                file_path TEXT NOT NULL,
                chunk_type TEXT NOT NULL DEFAULT 'text',
                content TEXT NOT NULL,
                heading TEXT NOT NULL DEFAULT '',
                chunk_index INTEGER NOT NULL DEFAULT 0,
                image_path TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            )
            "#,
        )
        .execute(pool)
        .await?;

        // Chunk embeddings table (vector storage)
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS chunk_embeddings (
                chunk_id INTEGER PRIMARY KEY REFERENCES doc_chunks(id) ON DELETE CASCADE,
                embedding BLOB NOT NULL
            )
            "#,
        )
        .execute(pool)
        .await?;

        // FTS5 virtual table for chunk full-text search
        sqlx::query(
            r#"
            CREATE VIRTUAL TABLE IF NOT EXISTS doc_chunks_fts USING fts5(
                content,
                heading,
                content='doc_chunks',
                content_rowid='id'
            )
            "#,
        )
        .execute(pool)
        .await?;

        // Triggers for chunk FTS sync
        sqlx::query(
            r#"
            CREATE TRIGGER IF NOT EXISTS doc_chunks_fts_insert AFTER INSERT ON doc_chunks BEGIN
                INSERT INTO doc_chunks_fts(rowid, content, heading)
                VALUES (NEW.id, NEW.content, NEW.heading);
            END
            "#,
        )
        .execute(pool)
        .await?;

        sqlx::query(
            r#"
            CREATE TRIGGER IF NOT EXISTS doc_chunks_fts_delete AFTER DELETE ON doc_chunks BEGIN
                INSERT INTO doc_chunks_fts(doc_chunks_fts, rowid, content, heading)
                VALUES ('delete', OLD.id, OLD.content, OLD.heading);
            END
            "#,
        )
        .execute(pool)
        .await?;

        // Indexes for chunk queries
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_doc_chunks_file ON doc_chunks(file_path)")
            .execute(pool)
            .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_doc_chunks_type ON doc_chunks(chunk_type)")
            .execute(pool)
            .await?;

        Ok(())
    }

    /// Add or update a document in the index.
    ///
    /// For PDF files, automatically parses the content using Docling service.
    /// For text files, reads the content directly.
    /// Excel files (xlsx/xls) are imported into a per-document SQLite database
    /// (see `excel_db` module) and a structural summary is indexed.
    /// Other binary files (docx, etc.) are indexed with metadata only.
    /// 
    /// For PDF and Markdown files, document chunks are also stored and embedded
    /// for semantic (vector) search.
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
        let mut doc_hash = String::new();
        let content_text = if ext == "pdf" {
            // Parse PDF using Docling service
            match self.parse_with_docling(&full_path.to_string_lossy(), file_path, workspace_root).await {
                Ok((markdown, hash)) => {
                    doc_hash = hash;
                    markdown
                }
                Err(e) => {
                    warn!(file = %file_path, err = %e, "Failed to parse PDF with Docling");
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
        } else if matches!(ext.as_str(), "xlsx" | "xls") {
            // Excel files: parse into a per-document SQLite database (one table
            // per sheet, every column indexed) and index a structural summary.
            match crate::excel_db::import_excel(&self.data_dir, &self.user_id, file_path, workspace_root).await {
                Ok(summary) => summary.to_index_text(),
                Err(e) => {
                    warn!(file = %file_path, err = %e, "Failed to import Excel into SQLite");
                    format!("[Excel import error: {}]", e)
                }
            }
        } else {
            // Binary files (docx, etc.) - no content extraction for now
            String::new()
        };

        // Truncate very long content (use char boundary to avoid cutting UTF-8 mid-character)
        let content_text = if content_text.len() > MAX_INDEX_CONTENT_LEN {
            let mut end = MAX_INDEX_CONTENT_LEN;
            // Walk back to find a valid char boundary
            while end > 0 && !content_text.is_char_boundary(end) {
                end -= 1;
            }
            let mut truncated = content_text[..end].to_string();
            truncated.push_str("\n\n[... content truncated for indexing ...]");
            truncated
        } else {
            content_text
        };

        // Upsert into database
        let now = chrono::Utc::now().to_rfc3339();

        sqlx::query(
            r#"
            INSERT INTO documents (file_path, dir_path, filename, content_type, size, content_text, doc_hash, indexed_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            ON CONFLICT(file_path) DO UPDATE SET
                dir_path = ?2,
                filename = ?3,
                content_type = ?4,
                size = ?5,
                content_text = ?6,
                doc_hash = ?7,
                indexed_at = ?8
            "#,
        )
        .bind(file_path)
        .bind(&dir_path)
        .bind(&filename)
        .bind(content_type)
        .bind(size)
        .bind(&content_text)
        .bind(&doc_hash)
        .bind(&now)
        .execute(&self.pool)
        .await?;

        // For PDF and Markdown files, also store chunks for vector search
        if matches!(ext.as_str(), "pdf" | "md" | "markdown") && !content_text.is_empty() {
            // Delete existing chunks first (in case of re-index)
            self.delete_file_chunks(file_path).await?;
            
            // Chunk the document and store with embeddings
            if let Err(e) = self.store_document_chunks(file_path, &content_text).await {
                warn!(file = %file_path, err = %e, "Failed to store document chunks for vector search");
                // Don't fail the whole operation - FTS still works
            }
        }

        info!(file = %file_path, content_type = %content_type, "Document indexed");
        Ok(())
    }

    /// Remove a file from the index.
    pub async fn remove_file(&self, file_path: &str) -> Result<()> {
        sqlx::query("DELETE FROM documents WHERE file_path = ?")
            .bind(file_path)
            .execute(&self.pool)
            .await?;

        // Also delete document chunks (cascades to embeddings via FK)
        self.delete_file_chunks(file_path).await?;

        // If it was an Excel file, also drop its imported SQLite database.
        let ext = file_path.rsplit('.').next().unwrap_or("").to_lowercase();
        if matches!(ext.as_str(), "xlsx" | "xls") {
            if let Err(e) = crate::excel_db::remove_db_for(&self.data_dir, &self.user_id, file_path).await {
                warn!(file = %file_path, err = %e, "Failed to remove Excel database");
            }
        }

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

        // Get file paths before deleting (to also delete chunks)
        let files: Vec<String> = sqlx::query_scalar(
            "SELECT file_path FROM documents WHERE dir_path = ?1 OR dir_path LIKE ?2"
        )
        .bind(dir_path)
        .bind(&pattern)
        .fetch_all(&self.pool)
        .await?;

        sqlx::query("DELETE FROM documents WHERE dir_path = ? OR dir_path LIKE ?")
            .bind(dir_path)
            .bind(&pattern)
            .execute(&self.pool)
            .await?;

        // Delete chunks for all removed files
        for file_path in files {
            let _ = self.delete_file_chunks(&file_path).await;
        }

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

    /// Get the Docling document hash for a file (used for image asset URLs).
    /// Returns None if the file is not indexed or has no doc_hash.
    pub async fn get_doc_hash(&self, file_path: &str) -> Result<Option<String>> {
        let row = sqlx::query_as::<_, (String,)>(
            "SELECT doc_hash FROM documents WHERE file_path = ?1"
        )
        .bind(file_path)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.and_then(|r| if r.0.is_empty() { None } else { Some(r.0) }))
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

    // ========== Docling Integration ==========

    /// Parse a PDF file using the Docling HTTP service.
    ///
    /// Returns the structured Markdown content and the document hash (for image assets).
    async fn parse_with_docling(&self, pdf_path: &str, file_path: &str, _workspace_root: &str) -> Result<(String, String)> {
        let service_url = std::env::var("DOCLING_SERVICE_URL")
            .unwrap_or_else(|_| "http://localhost:50060".to_string());
        
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(180))
            .build()?;
        
        // Read the PDF file
        let file_content = tokio::fs::read(pdf_path).await?;
        
        // Send to Docling service
        let form = reqwest::multipart::Form::new()
            .part("file", reqwest::multipart::Part::bytes(file_content)
                .file_name(Path::new(pdf_path).file_name().unwrap_or_default().to_string_lossy().to_string())
                .mime_str("application/pdf")?);
        
        let response = client
            .post(format!("{}/convert", service_url))
            .multipart(form)
            .send()
            .await?;
        
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Docling service error ({}): {}", status, body);
        }
        
        // Parse response
        #[derive(serde::Deserialize)]
        #[allow(dead_code)]
        struct ConvertResponse {
            markdown: String,
            tables: Vec<String>,
            images: Vec<serde_json::Value>,
            metadata: serde_json::Value,
        }
        
        let result: ConvertResponse = response.json().await?;
        
        // Extract doc_hash from metadata (used for image asset URLs)
        let doc_hash = result.metadata.get("doc_hash")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        
        info!(
            file = %file_path,
            tables = result.tables.len(),
            images = result.images.len(),
            doc_hash = %doc_hash,
            "PDF parsed with Docling"
        );
        
        Ok((result.markdown, doc_hash))
    }

    // ========== Vector Search (Chunk Storage) ==========

    /// Store document chunks with embeddings for vector search.
    async fn store_document_chunks(&self, file_path: &str, content: &str) -> Result<()> {
        // Chunk the document
        let chunks = chunk_markdown(content, file_path);
        
        if chunks.is_empty() {
            return Ok(());
        }
        
        // Get embedding client
        let embedding_client = EmbeddingClient::from_env()?;
        
        // Check if service is available
        if !embedding_client.health_check().await.unwrap_or(false) {
            warn!("Embedding service not available, skipping vector indexing");
            return Ok(());
        }
        
        // Prepare texts for embedding
        let texts: Vec<String> = chunks.iter().map(|c| {
            format!("{}\n{}", c.heading, c.content)
        }).collect();
        let text_refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
        
        // Generate embeddings
        let embeddings = embedding_client.embed_batch(&text_refs).await?;
        
        // Store chunks and embeddings
        for (chunk, embedding) in chunks.into_iter().zip(embeddings.into_iter()) {
            // Insert chunk
            let chunk_id: i64 = sqlx::query_scalar(
                r#"
                INSERT INTO doc_chunks (file_path, chunk_type, content, heading, chunk_index, image_path)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                RETURNING id
                "#
            )
            .bind(&chunk.file_path)
            .bind(&chunk.chunk_type)
            .bind(&chunk.content)
            .bind(&chunk.heading)
            .bind(chunk.chunk_index)
            .bind(&chunk.image_path)
            .fetch_one(&self.pool)
            .await?;
            
            // Insert embedding
            let embedding_bytes = embedding_to_bytes(&embedding);
            sqlx::query(
                "INSERT INTO chunk_embeddings (chunk_id, embedding) VALUES (?1, ?2)"
            )
            .bind(chunk_id)
            .bind(&embedding_bytes)
            .execute(&self.pool)
            .await?;
        }
        
        info!(file = %file_path, "Stored document chunks for vector search");
        Ok(())
    }

    /// Delete all chunks for a file.
    pub async fn delete_file_chunks(&self, file_path: &str) -> Result<()> {
        // Get chunk IDs first (for cascade delete of embeddings)
        let chunk_ids: Vec<i64> = sqlx::query_scalar(
            "SELECT id FROM doc_chunks WHERE file_path = ?1"
        )
        .bind(file_path)
        .fetch_all(&self.pool)
        .await?;
        
        if chunk_ids.is_empty() {
            return Ok(());
        }
        
        // Delete embeddings
        for chunk_id in &chunk_ids {
            sqlx::query("DELETE FROM chunk_embeddings WHERE chunk_id = ?1")
                .bind(chunk_id)
                .execute(&self.pool)
                .await?;
        }
        
        // Delete chunks
        sqlx::query("DELETE FROM doc_chunks WHERE file_path = ?1")
            .bind(file_path)
            .execute(&self.pool)
            .await?;
        
        Ok(())
    }

    /// Get all chunks for a file.
    pub async fn get_file_chunks(&self, file_path: &str) -> Result<Vec<DocChunk>> {
        let chunks = sqlx::query_as::<_, DocChunkRow>(
            r#"
            SELECT id, file_path, chunk_type, content, heading, chunk_index, image_path, created_at
            FROM doc_chunks
            WHERE file_path = ?1
            ORDER BY chunk_index
            "#
        )
        .bind(file_path)
        .fetch_all(&self.pool)
        .await?;
        
        Ok(chunks.into_iter().map(|r| r.into()).collect())
    }

    /// Perform vector similarity search.
    ///
    /// Returns chunks ranked by cosine similarity to the query embedding.
    pub async fn vector_search(
        &self,
        query_embedding: &[f32],
        top_k: u32,
        file_path_filter: Option<&str>,
    ) -> Result<Vec<ScoredChunk>> {
        // Load all embeddings (for small document collections this is fast)
        let rows = if let Some(fp) = file_path_filter {
            sqlx::query_as::<_, ChunkEmbeddingRow>(
                r#"
                SELECT c.id, c.file_path, c.chunk_type, c.content, c.heading, c.chunk_index, c.image_path, e.embedding
                FROM chunk_embeddings e
                JOIN doc_chunks c ON c.id = e.chunk_id
                WHERE c.file_path = ?1
                "#
            )
            .bind(fp)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query_as::<_, ChunkEmbeddingRow>(
                r#"
                SELECT c.id, c.file_path, c.chunk_type, c.content, c.heading, c.chunk_index, c.image_path, e.embedding
                FROM chunk_embeddings e
                JOIN doc_chunks c ON c.id = e.chunk_id
                "#
            )
            .fetch_all(&self.pool)
            .await?
        };
        
        // Compute similarities
        let mut scored: Vec<ScoredChunk> = rows
            .into_iter()
            .map(|row| {
                let chunk_embedding = bytes_to_embedding(&row.embedding);
                let score = cosine_similarity(query_embedding, &chunk_embedding);
                ScoredChunk {
                    id: row.id,
                    file_path: row.file_path,
                    chunk_type: row.chunk_type,
                    content: row.content,
                    heading: row.heading,
                    chunk_index: row.chunk_index,
                    image_path: row.image_path,
                    score,
                }
            })
            .collect();
        
        // Sort by score descending
        scored.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        
        // Take top_k
        scored.truncate(top_k as usize);
        
        Ok(scored)
    }

    /// Perform hybrid search (FTS5 + vector).
    ///
    /// Combines keyword search with semantic search for best results.
    pub async fn hybrid_search(
        &self,
        query: &str,
        top_k: u32,
        file_paths: Option<&[String]>,
    ) -> Result<Vec<ScoredChunk>> {
        tracing::info!("Creating embedding client");
        let embedding_client = EmbeddingClient::from_env()?;
        
        tracing::info!("Checking embedding service health");
        // Check if embedding service is available
        let health = embedding_client.health_check().await.unwrap_or(false);
        tracing::info!(health_ok = health, "Health check result");
        
        if health {
            // Use vector search
            tracing::info!("Generating query embedding");
            let query_embedding = embedding_client.embed_query(query).await?;
            tracing::info!(embedding_dim = query_embedding.len(), "Query embedding generated");
            
            if let Some(paths) = file_paths {
                // Search within specific files
                let mut all_results = Vec::new();
                for path in paths {
                    let results = self.vector_search(&query_embedding, top_k, Some(path)).await?;
                    all_results.extend(results);
                }
                all_results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
                all_results.truncate(top_k as usize);
                Ok(all_results)
            } else {
                self.vector_search(&query_embedding, top_k, None).await
            }
        } else {
            // Fall back to FTS5 search
            warn!("Embedding service not available, falling back to FTS5 search");
            self.fts_chunk_search(query, top_k).await
        }
    }

    /// FTS5-based chunk search (fallback when embedding service is unavailable).
    async fn fts_chunk_search(&self, query: &str, top_k: u32) -> Result<Vec<ScoredChunk>> {
        let rows = sqlx::query_as::<_, DocChunkRow>(
            r#"
            SELECT c.id, c.file_path, c.chunk_type, c.content, c.heading, c.chunk_index, c.image_path, c.created_at
            FROM doc_chunks_fts fts
            JOIN doc_chunks c ON c.id = fts.rowid
            WHERE doc_chunks_fts MATCH ?1
            ORDER BY rank
            LIMIT ?2
            "#
        )
        .bind(query)
        .bind(top_k as i64)
        .fetch_all(&self.pool)
        .await?;
        
        Ok(rows.into_iter().map(|r| {
            let chunk: DocChunk = r.into();
            ScoredChunk {
                id: chunk.chunk_index as i64, // Use chunk_index as pseudo-id
                file_path: chunk.file_path,
                chunk_type: chunk.chunk_type,
                content: chunk.content,
                heading: chunk.heading,
                chunk_index: chunk.chunk_index,
                image_path: chunk.image_path,
                score: 1.0, // FTS results have equal score
            }
        }).collect())
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

/// Row type for doc_chunks queries.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct DocChunkRow {
    pub id: i64,
    pub file_path: String,
    pub chunk_type: String,
    pub content: String,
    pub heading: String,
    pub chunk_index: i32,
    pub image_path: Option<String>,
    pub created_at: String,
}

impl From<DocChunkRow> for DocChunk {
    fn from(row: DocChunkRow) -> Self {
        DocChunk {
            file_path: row.file_path,
            chunk_type: row.chunk_type,
            content: row.content,
            heading: row.heading,
            chunk_index: row.chunk_index,
            image_path: row.image_path,
        }
    }
}

/// Row type for chunk + embedding queries.
#[derive(Debug, sqlx::FromRow)]
struct ChunkEmbeddingRow {
    id: i64,
    file_path: String,
    chunk_type: String,
    content: String,
    heading: String,
    chunk_index: i32,
    image_path: Option<String>,
    embedding: Vec<u8>,
}

/// A document chunk with a relevance score.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ScoredChunk {
    pub id: i64,
    pub file_path: String,
    pub chunk_type: String,
    pub content: String,
    pub heading: String,
    pub chunk_index: i32,
    pub image_path: Option<String>,
    pub score: f32,
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

    #[tokio::test]
    async fn test_add_excel_imports_sqlite_database() {
        use rust_xlsxwriter::Workbook;

        let (index, dir) = setup_test_index().await;
        let workspace = dir.path().join("test-workspace");
        tokio::fs::create_dir_all(&workspace).await.unwrap();

        // Build a real .xlsx on disk
        let mut wb = Workbook::new();
        let ws = wb.add_worksheet().set_name("库存").unwrap();
        ws.write_string(0, 0, "商品").unwrap();
        ws.write_string(0, 1, "数量").unwrap();
        ws.write_string(1, 0, "苹果").unwrap();
        ws.write_number(1, 1, 42).unwrap();
        wb.save(workspace.join("库存表.xlsx")).unwrap();

        let ws_str = workspace.to_string_lossy().to_string();
        index.add_document("库存表.xlsx", &ws_str).await.unwrap();

        // The FTS index holds the structural summary
        let content = index.get_content("库存表.xlsx").await.unwrap().unwrap();
        assert!(content.contains("库存"), "summary should mention the table name");
        assert!(content.contains("库存表.xlsx"));

        // The SQLite database was created with one table, two indexed columns
        let db_path = crate::excel_db::db_path_for(
            dir.path().to_str().unwrap(),
            "test-user",
            "库存表.xlsx",
        );
        let info = crate::excel_db::describe_database(&db_path).await.unwrap();
        assert_eq!(info.len(), 1);
        assert_eq!(info[0].name, "库存");
        assert_eq!(info[0].row_count, 1);
        assert_eq!(info[0].indexes.len(), 2);

        // Removing the file also removes the database
        index.remove_file("库存表.xlsx").await.unwrap();
        assert!(!db_path.exists());
    }
}
