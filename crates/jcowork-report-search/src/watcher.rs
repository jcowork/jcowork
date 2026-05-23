//! Background file watcher and indexing pipeline.
//!
//! Polls the reports directory every 30 seconds for new PDF files.
//! For each new file (not in DB by path+hash), extracts text via pdftext,
//! splits into chunks, and stores everything in the SQLite index.

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::parser;

/// Infer document type from filename keywords.
fn infer_doc_type(filename: &str) -> &'static str {
    let lower = filename.to_lowercase();
    if lower.contains("年报") || lower.contains("年度报告") || lower.contains("annual") {
        "年报"
    } else if lower.contains("季报") || lower.contains("季度报告") || lower.contains("半年报") || lower.contains("半年度报告") || lower.contains("quarterly") {
        "季报"
    } else if lower.contains("研报") || lower.contains("研究报告") || lower.contains("报告") {
        "研报"
    } else if lower.contains("招股") || lower.contains("prospectus") {
        "招股书"
    } else {
        "other"
    }
}

/// Extract 4-digit year from filename, e.g. "2024".
fn infer_year(filename: &str) -> Option<i64> {
    // Collect chars to avoid byte-boundary issues with multi-byte characters
    let chars: Vec<char> = filename.chars().collect();
    for i in 0..chars.len().saturating_sub(3) {
        let s: String = chars[i..i + 4].iter().collect();
        if let Ok(y) = s.parse::<i64>() {
            if (2000..=2099).contains(&y) {
                return Some(y);
            }
        }
    }
    None
}

/// Compute SHA-256 hex digest of file bytes.
async fn file_hash(path: &str) -> Result<String> {
    let bytes = tokio::fs::read(path).await?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(hex::encode(hasher.finalize()))
}

/// Check if this file is already indexed (by file_path AND hash).
async fn is_indexed(pool: &SqlitePool, file_path: &str, hash: &str) -> bool {
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT file_hash FROM documents WHERE file_path = ?",
    )
    .bind(file_path)
    .fetch_optional(pool)
    .await
    .unwrap_or(None);

    match row {
        Some((existing_hash,)) => existing_hash == hash,
        None => false,
    }
}

/// Index a single PDF file into the database.
pub async fn index_file(pool: &SqlitePool, file_path: &str, company: &str) -> Result<()> {
    let hash = file_hash(file_path).await?;
    if is_indexed(pool, file_path, &hash).await {
        return Ok(()); // Already indexed and unchanged
    }

    let filename = Path::new(file_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(file_path);

    let doc_type = infer_doc_type(filename);
    let year = infer_year(filename);

    info!(file = %filename, company = %company, doc_type = %doc_type, "Indexing PDF");

    // Extract text via pdftext
    let text = parser::extract_text(file_path).await?;

    // Scanned / image-only PDFs produce no extractable text.
    // Record them in the DB with total_chunks=0 and doc_type='skip' so the watcher
    // doesn't retry them on every 30s cycle.
    if text.trim().is_empty() {
        warn!(file = %filename, "Skipping image-only/scanned PDF (no extractable text)");
        // Remove any stale record first
        sqlx::query("DELETE FROM documents WHERE file_path = ?")
            .bind(file_path)
            .execute(pool)
            .await?;
        let doc_id = Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO documents (id, company, filename, file_path, doc_type, year, file_hash, parsed_at, total_chunks)
             VALUES (?, ?, ?, ?, 'skip', ?, ?, ?, 0)",
        )
        .bind(&doc_id)
        .bind(company)
        .bind(filename)
        .bind(file_path)
        .bind(year)
        .bind(&hash)
        .bind(&now)
        .execute(pool)
        .await?;
        return Ok(());
    }

    let chunks = parser::split_into_chunks(&text);
    let chunk_count = chunks.len();

    if chunk_count == 0 {
        warn!(file = %filename, "No chunks extracted, skipping");
        return Ok(());
    }

    // Remove old document record if hash changed (re-parse)
    sqlx::query("DELETE FROM documents WHERE file_path = ?")
        .bind(file_path)
        .execute(pool)
        .await?;

    // Insert new document record
    let doc_id = Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO documents (id, company, filename, file_path, doc_type, year, file_hash, parsed_at, total_chunks)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&doc_id)
    .bind(company)
    .bind(filename)
    .bind(file_path)
    .bind(doc_type)
    .bind(year)
    .bind(&hash)
    .bind(&now)
    .bind(chunk_count as i64)
    .execute(pool)
    .await?;

    // Insert chunks (batched via transaction for performance)
    let mut tx = pool.begin().await?;
    for (idx, chunk_text) in chunks.iter().enumerate() {
        let chunk_id = Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO chunks (id, doc_id, chunk_index, content) VALUES (?, ?, ?, ?)",
        )
        .bind(&chunk_id)
        .bind(&doc_id)
        .bind(idx as i64)
        .bind(chunk_text)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;

    info!(file = %filename, chunks = chunk_count, "Indexed successfully");
    Ok(())
}

/// Scan a reports directory for PDF files and index any new ones.
/// The directory structure is: reports_dir/{company_name}/{filename}.pdf
pub async fn scan_and_index(pool: Arc<SqlitePool>, reports_dir: &str) -> Result<()> {
    let reports_path = Path::new(reports_dir);
    if !reports_path.exists() {
        warn!(dir = %reports_dir, "Reports directory does not exist, skipping scan");
        return Ok(());
    }

    // Each subdirectory is a company name
    let mut company_dir_entries = tokio::fs::read_dir(reports_path).await?;
    while let Some(company_entry) = company_dir_entries.next_entry().await? {
        let company_path = company_entry.path();
        if !company_path.is_dir() {
            continue;
        }
        let company = company_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        if company.is_empty() || company.starts_with('.') {
            continue;
        }

        // Scan PDF files in this company directory (recursive)
        scan_company_dir(&pool, &company_path.to_string_lossy(), &company).await;
    }

    Ok(())
}

/// Recursively scan a company directory for PDFs and index them.
async fn scan_company_dir(pool: &SqlitePool, dir: &str, company: &str) {
    let dir_path = Path::new(dir);
    let mut entries = match tokio::fs::read_dir(dir_path).await {
        Ok(e) => e,
        Err(e) => {
            error!(dir = %dir, err = %e, "Failed to read directory");
            return;
        }
    };

    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if path.is_dir() {
            Box::pin(scan_company_dir(pool, &path.to_string_lossy(), company)).await;
        } else if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf")) {
            let file_path = path.to_string_lossy().to_string();
            if let Err(e) = index_file(pool, &file_path, company).await {
                error!(file = %file_path, err = %e, "Failed to index PDF");
            }
        }
    }
}

/// Background task: polls the reports directory every `interval_secs` seconds.
pub async fn run_watcher(pool: Arc<SqlitePool>, reports_dir: String, interval_secs: u64) {
    info!(dir = %reports_dir, interval = interval_secs, "File watcher started");

    // Initial scan immediately on startup
    if let Err(e) = scan_and_index(pool.clone(), &reports_dir).await {
        error!(err = %e, "Initial scan failed");
    }

    // Periodic polling loop
    let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(interval_secs));
    interval.tick().await; // consume the first tick (already scanned)

    loop {
        interval.tick().await;
        if let Err(e) = scan_and_index(pool.clone(), &reports_dir).await {
            error!(err = %e, "Periodic scan failed");
        }
    }
}
