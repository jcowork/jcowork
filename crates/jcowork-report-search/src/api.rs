//! HTTP API handlers for the report search service.

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use std::sync::Arc;
use tracing::info;

use crate::watcher;

/// Shared application state.
#[derive(Clone)]
pub struct AppState {
    pub pool: Arc<SqlitePool>,
    pub reports_dir: String,
}

/// Build the Axum router.
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/search", get(search))
        .route("/companies", get(companies))
        .route("/documents", get(documents))
        .route("/index", post(trigger_index))
        .with_state(state)
}

// ── Health ────────────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct HealthResponse {
    ok: bool,
    indexed_documents: i64,
    indexed_chunks: i64,
}

async fn health(State(state): State<AppState>) -> impl IntoResponse {
    let doc_count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM documents")
        .fetch_one(state.pool.as_ref())
        .await
        .unwrap_or((0,));
    let chunk_count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM chunks")
        .fetch_one(state.pool.as_ref())
        .await
        .unwrap_or((0,));

    Json(HealthResponse {
        ok: true,
        indexed_documents: doc_count.0,
        indexed_chunks: chunk_count.0,
    })
}

// ── Search ────────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct SearchQuery {
    pub q: String,
    pub company: Option<String>,
    pub doc_type: Option<String>,
    pub limit: Option<i64>,
}

#[derive(Serialize, sqlx::FromRow)]
pub struct SearchResult {
    pub doc_id: String,
    pub company: String,
    pub filename: String,
    pub doc_type: String,
    pub year: Option<i64>,
    pub chunk: String,
    pub score: f64,
}

async fn search(
    State(state): State<AppState>,
    Query(params): Query<SearchQuery>,
) -> impl IntoResponse {
    let limit = params.limit.unwrap_or(20).min(100);

    // Escape FTS5 special characters in the query
    let fts_query = escape_fts5_query(&params.q);

    // Build SQL with optional company/doc_type filters
    // FTS5 rank() returns negative BM25 score; we negate to get positive
    let results = match (&params.company, &params.doc_type) {
        (Some(company), Some(doc_type)) => {
            sqlx::query_as::<_, SearchResult>(r#"
                SELECT d.id as doc_id, d.company, d.filename, d.doc_type, d.year,
                       c.content as chunk, (-rank) as score
                FROM chunks_fts f
                JOIN chunks c ON c.rowid = f.rowid
                JOIN documents d ON d.id = c.doc_id
                WHERE chunks_fts MATCH ? AND d.company = ? AND d.doc_type = ?
                ORDER BY rank
                LIMIT ?
            "#)
            .bind(&fts_query)
            .bind(company)
            .bind(doc_type)
            .bind(limit)
            .fetch_all(state.pool.as_ref())
            .await
        }
        (Some(company), None) => {
            sqlx::query_as::<_, SearchResult>(r#"
                SELECT d.id as doc_id, d.company, d.filename, d.doc_type, d.year,
                       c.content as chunk, (-rank) as score
                FROM chunks_fts f
                JOIN chunks c ON c.rowid = f.rowid
                JOIN documents d ON d.id = c.doc_id
                WHERE chunks_fts MATCH ? AND d.company = ?
                ORDER BY rank
                LIMIT ?
            "#)
            .bind(&fts_query)
            .bind(company)
            .bind(limit)
            .fetch_all(state.pool.as_ref())
            .await
        }
        (None, Some(doc_type)) => {
            sqlx::query_as::<_, SearchResult>(r#"
                SELECT d.id as doc_id, d.company, d.filename, d.doc_type, d.year,
                       c.content as chunk, (-rank) as score
                FROM chunks_fts f
                JOIN chunks c ON c.rowid = f.rowid
                JOIN documents d ON d.id = c.doc_id
                WHERE chunks_fts MATCH ? AND d.doc_type = ?
                ORDER BY rank
                LIMIT ?
            "#)
            .bind(&fts_query)
            .bind(doc_type)
            .bind(limit)
            .fetch_all(state.pool.as_ref())
            .await
        }
        (None, None) => {
            sqlx::query_as::<_, SearchResult>(r#"
                SELECT d.id as doc_id, d.company, d.filename, d.doc_type, d.year,
                       c.content as chunk, (-rank) as score
                FROM chunks_fts f
                JOIN chunks c ON c.rowid = f.rowid
                JOIN documents d ON d.id = c.doc_id
                WHERE chunks_fts MATCH ?
                ORDER BY rank
                LIMIT ?
            "#)
            .bind(&fts_query)
            .bind(limit)
            .fetch_all(state.pool.as_ref())
            .await
        }
    };

    match results {
        Ok(rows) => (StatusCode::OK, Json(rows)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// Escape FTS5 special characters and wrap each token for prefix matching.
fn escape_fts5_query(raw: &str) -> String {
    // Split on whitespace, wrap each token in quotes to treat as phrase
    let tokens: Vec<String> = raw
        .split_whitespace()
        .filter(|t| !t.is_empty())
        .map(|t| {
            // Escape double quotes inside token
            let escaped = t.replace('"', "\"\"");
            format!("\"{}\"", escaped)
        })
        .collect();
    if tokens.is_empty() {
        "\"\"".to_string()
    } else {
        tokens.join(" OR ")
    }
}

// ── Companies ─────────────────────────────────────────────────────────────────

async fn companies(State(state): State<AppState>) -> impl IntoResponse {
    let rows: Vec<(String,)> =
        sqlx::query_as("SELECT DISTINCT company FROM documents ORDER BY company")
            .fetch_all(state.pool.as_ref())
            .await
            .unwrap_or_default();
    let names: Vec<String> = rows.into_iter().map(|(c,)| c).collect();
    Json(names)
}

// ── Documents ─────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct DocumentsQuery {
    company: Option<String>,
}

#[derive(Serialize, sqlx::FromRow)]
struct DocumentInfo {
    id: String,
    company: String,
    filename: String,
    doc_type: String,
    year: Option<i64>,
    parsed_at: String,
    total_chunks: i64,
}

async fn documents(
    State(state): State<AppState>,
    Query(params): Query<DocumentsQuery>,
) -> impl IntoResponse {
    let rows: Result<Vec<DocumentInfo>, _> = match &params.company {
        Some(company) => sqlx::query_as(
            "SELECT id, company, filename, doc_type, year, parsed_at, total_chunks
             FROM documents WHERE company = ? ORDER BY year DESC, filename",
        )
        .bind(company)
        .fetch_all(state.pool.as_ref())
        .await,
        None => sqlx::query_as(
            "SELECT id, company, filename, doc_type, year, parsed_at, total_chunks
             FROM documents ORDER BY company, year DESC",
        )
        .fetch_all(state.pool.as_ref())
        .await,
    };

    match rows {
        Ok(docs) => Json(docs).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

// ── Manual re-index trigger ───────────────────────────────────────────────────

async fn trigger_index(State(state): State<AppState>) -> impl IntoResponse {
    let pool = state.pool.clone();
    let reports_dir = state.reports_dir.clone();

    // Run in background so the HTTP response returns immediately
    tokio::spawn(async move {
        info!("Manual re-index triggered");
        if let Err(e) = watcher::scan_and_index(pool, &reports_dir).await {
            tracing::error!(err = %e, "Manual re-index failed");
        }
    });

    Json(serde_json::json!({ "status": "indexing started" }))
}
