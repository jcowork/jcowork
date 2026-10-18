//! Aggregated "AI feed" endpoints.
//!
//! The feed merges, per user, their periodic-task execution results with
//! their (public-only when viewing someone else) indexed documents, newest
//! first, with offset pagination. The mobile "AI头条" tab fans these
//! endpoints out across all locally signed-in accounts and public users and
//! merges the streams client-side.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Extension, Json,
};
use chrono::{DateTime, TimeZone, Utc};
use serde::Deserialize;

use super::{AppState, AuthUser};
use jcowork_storage::WorkspaceIndex;

#[derive(Debug, Deserialize)]
pub(crate) struct FeedQuery {
    offset: Option<i64>,
    limit: Option<i64>,
}

/// One feed entry: a task execution result or an indexed document.
#[derive(Debug, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum FeedItem {
    Task {
        id: String,
        job_id: String,
        job_name: Option<String>,
        prompt: Option<String>,
        /// First ~400 characters of the output (truncated in SQL).
        excerpt: String,
        /// AI teaser summary; empty until generated (client falls back to excerpt).
        summary: String,
        status: String,
        /// RFC3339 UTC.
        executed_at: String,
    },
    Document {
        file_path: String,
        dir_path: String,
        filename: String,
        content_type: String,
        size: i64,
        snippet: String,
        /// AI teaser summary; empty until generated (client falls back to snippet).
        summary: String,
        /// Owner-marked public flag (share eligibility for the standalone link).
        #[serde(rename = "is_public")]
        is_public: bool,
        /// RFC3339 UTC (normalized from SQLite's naive format).
        indexed_at: String,
    },
}

#[derive(serde::Serialize)]
pub(crate) struct FeedResponse {
    items: Vec<FeedItem>,
    next_offset: i64,
    has_more: bool,
}

/// Parse a stored timestamp as UTC. Task `executed_at` values are RFC3339;
/// document `indexed_at` values are SQLite's naive UTC "YYYY-MM-DD HH:MM:SS".
fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&Utc));
    }
    chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
        .ok()
        .map(|n| Utc.from_utc_datetime(&n))
}

/// Render a stored timestamp as RFC3339 UTC (unparseable values pass through).
fn to_rfc3339_utc(s: &str) -> String {
    match parse_ts(s) {
        Some(dt) => dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        None => s.to_string(),
    }
}

/// Merge one user's task results and documents into a single time-sorted page.
///
/// Both sources are fetched with `offset + limit` rows and merged before
/// slicing `[offset, offset + limit)`, so the window honors the global order
/// even when the two tables interleave. `has_more` is true when either source
/// saturated its fetch — the tail may then cost one extra (empty) request,
/// which is the price of not running a cursor protocol.
async fn build_feed(
    state: &AppState,
    user_id: &str,
    public_docs_only: bool,
    limit: i64,
    offset: i64,
) -> Result<FeedResponse, anyhow::Error> {
    let limit = limit.clamp(1, 50);
    let offset = offset.max(0);
    let fetch = offset + limit;

    let tasks = state.cron_scheduler.list_user_task_results(user_id, fetch, 0).await;
    let index = WorkspaceIndex::cached(&state.data_dir, user_id).await?;
    let docs = if public_docs_only {
        index.list_public_recent(fetch, 0).await?
    } else {
        index.list_recent(fetch, 0).await?
    };

    let has_more = tasks.len() as i64 >= fetch || docs.len() as i64 >= fetch;

    let mut merged: Vec<(DateTime<Utc>, FeedItem)> = Vec::with_capacity(tasks.len() + docs.len());
    for t in tasks {
        let ts = parse_ts(&t.executed_at).unwrap_or(DateTime::<Utc>::UNIX_EPOCH);
        merged.push((
            ts,
            FeedItem::Task {
                id: t.id,
                job_id: t.cron_job_id,
                job_name: t.job_name,
                prompt: t.job_prompt,
                excerpt: t.output,
                summary: t.summary,
                status: t.status,
                executed_at: to_rfc3339_utc(&t.executed_at),
            },
        ));
    }
    for d in docs {
        let ts = parse_ts(&d.indexed_at).unwrap_or(DateTime::<Utc>::UNIX_EPOCH);
        merged.push((
            ts,
            FeedItem::Document {
                file_path: d.file_path,
                dir_path: d.dir_path,
                filename: d.filename,
                content_type: d.content_type,
                size: d.size,
                snippet: d.snippet,
                summary: d.summary,
                is_public: d.is_public,
                indexed_at: to_rfc3339_utc(&d.indexed_at),
            },
        ));
    }
    // Newest first; ties keep tasks ahead of documents (stable sort by type).
    merged.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| match (&a.1, &b.1) {
                (FeedItem::Task { .. }, FeedItem::Document { .. }) => std::cmp::Ordering::Less,
                (FeedItem::Document { .. }, FeedItem::Task { .. }) => std::cmp::Ordering::Greater,
                _ => std::cmp::Ordering::Equal,
            })
    });

    let items: Vec<FeedItem> = merged
        .into_iter()
        .skip(offset as usize)
        .take(limit as usize)
        .map(|(_, item)| item)
        .collect();
    let next_offset = offset + items.len() as i64;

    // Lazy backfill: rows predating AI summaries get one generated on first
    // sight. Fire-and-forget — this page still serves the raw truncation and
    // the next view picks the stored summary up.
    for item in &items {
        match item {
            FeedItem::Task { id, summary, .. } if summary.is_empty() => {
                state.summary.ensure_task_summary(state.cron_scheduler.clone(), id);
            }
            FeedItem::Document { file_path, summary, .. } if summary.is_empty() => {
                state.summary.ensure_doc_summary(&state.data_dir, user_id, file_path);
            }
            _ => {}
        }
    }

    Ok(FeedResponse { items, next_offset, has_more })
}

/// GET /api/feed — the authenticated user's own merged feed.
pub(crate) async fn get_own_feed(
    State(state): State<AppState>,
    Extension(auth_user): Extension<AuthUser>,
    Query(query): Query<FeedQuery>,
) -> Response {
    match build_feed(
        &state,
        &auth_user.user_id,
        false,
        query.limit.unwrap_or(20),
        query.offset.unwrap_or(0),
    )
    .await
    {
        Ok(resp) => (StatusCode::OK, Json(resp)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// GET /api/public-users/{user_id}/feed — a public account's merged feed
/// (public documents only).
pub(crate) async fn get_public_feed(
    State(state): State<AppState>,
    Path(user_id): Path<String>,
    Query(query): Query<FeedQuery>,
) -> Response {
    if let Err(resp) = super::public_users::ensure_public_user(&state, &user_id).await {
        return resp;
    }
    match build_feed(
        &state,
        &user_id,
        true,
        query.limit.unwrap_or(20),
        query.offset.unwrap_or(0),
    )
    .await
    {
        Ok(resp) => (StatusCode::OK, Json(resp)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// GET /api/feed/results/{result_id} — one task result with full output.
///
/// Visible to the result's owner, or to anyone when the owner is a public
/// account (same gate as the other public-user reads).
pub(crate) async fn get_feed_result(
    State(state): State<AppState>,
    Extension(auth_user): Extension<AuthUser>,
    Path(result_id): Path<String>,
) -> Response {
    let entry = match state.cron_scheduler.get_task_result_by_id(&result_id).await {
        Some(e) => e,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": "Task result not found" })),
            )
                .into_response()
        }
    };
    if entry.user_id != auth_user.user_id {
        if let Err(resp) = super::public_users::ensure_public_user(&state, &entry.user_id).await {
            return resp;
        }
    }
    (StatusCode::OK, Json(serde_json::json!({
        "id": entry.id,
        "job_id": entry.cron_job_id,
        "user_id": entry.user_id,
        "output": entry.output,
        "status": entry.status,
        "executed_at": to_rfc3339_utc(&entry.executed_at),
        "job_name": entry.job_name,
        "prompt": entry.job_prompt,
    })))
    .into_response()
}
