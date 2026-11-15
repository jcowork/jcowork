//! RSI (recursive self-improvement) report storage.
//!
//! Every RSI run produces one row: the admin run analyses the deployment's
//! daily usage and proposes project-code improvements, a normal user run
//! improves that user's workspace workflows and periodic plans. Reports are
//! kept so the toggle page can show what the agent changed without needing
//! the workspace file.

use anyhow::Result;
use serde::Serialize;
use sqlx::SqlitePool;

/// A stored RSI run. `output` holds the full agent answer; list endpoints
/// return [`RsiReportSummary`] instead so one long report cannot bloat the
/// sidebar page payload.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct RsiReport {
    pub id: String,
    pub user_id: String,
    /// "admin" or "user" — the improvement scope the run followed.
    pub role: String,
    /// Start of the analysed window (RFC 3339, UTC).
    pub period_start: String,
    /// End of the analysed window (RFC 3339, UTC).
    pub period_end: String,
    /// Short teaser shown in the report list.
    pub summary: String,
    pub output: String,
    pub status: String,
    pub created_at: String,
}

/// List projection of [`RsiReport`] — everything except the full output.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct RsiReportSummary {
    pub id: String,
    pub user_id: String,
    pub role: String,
    pub period_start: String,
    pub period_end: String,
    pub summary: String,
    pub status: String,
    pub created_at: String,
}

/// Per-user counts of periodic-task executions in a time window.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct CronTaskStat {
    pub user_id: String,
    pub status: String,
    pub count: i64,
}

pub struct RsiStore {
    pool: SqlitePool,
}

impl RsiStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Persist a completed RSI run.
    pub async fn insert_report(&self, report: &RsiReport) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO rsi_reports
                (id, user_id, role, period_start, period_end, summary, output, status, created_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, COALESCE(NULLIF(?9, ''), datetime('now')))
            "#,
        )
        .bind(&report.id)
        .bind(&report.user_id)
        .bind(&report.role)
        .bind(&report.period_start)
        .bind(&report.period_end)
        .bind(&report.summary)
        .bind(&report.output)
        .bind(&report.status)
        .bind(&report.created_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Newest reports for a user, without the full output.
    pub async fn list_reports(&self, user_id: &str, limit: i64) -> Result<Vec<RsiReportSummary>> {
        let rows = sqlx::query_as::<_, RsiReportSummary>(
            r#"
            SELECT id, user_id, role, period_start, period_end, summary, status, created_at
            FROM rsi_reports
            WHERE user_id = ?1
            ORDER BY created_at DESC, rowid DESC
            LIMIT ?2
            "#,
        )
        .bind(user_id)
        .bind(limit.clamp(1, 100))
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// One report, scoped to its owner so an id from another account cannot
    /// be read by guessing.
    pub async fn get_report(&self, user_id: &str, id: &str) -> Result<Option<RsiReport>> {
        let row = sqlx::query_as::<_, RsiReport>(
            r#"
            SELECT id, user_id, role, period_start, period_end, summary, output, status, created_at
            FROM rsi_reports
            WHERE user_id = ?1 AND id = ?2
            "#,
        )
        .bind(user_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    /// `created_at` of the newest report, used by the daily scheduler to
    /// decide whether today's run already happened.
    pub async fn latest_report_at(&self, user_id: &str) -> Result<Option<String>> {
        let row = sqlx::query_as::<_, (String,)>(
            "SELECT created_at FROM rsi_reports WHERE user_id = ?1 ORDER BY created_at DESC, rowid DESC LIMIT 1",
        )
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|(c,)| c))
    }

    /// Success/error counts of periodic-task executions since `since`
    /// (RFC 3339), grouped by user.
    pub async fn cron_task_stats_since(&self, since: &str) -> Result<Vec<CronTaskStat>> {
        let rows = sqlx::query_as::<_, CronTaskStat>(
            r#"
            SELECT user_id, status, COUNT(*) AS count
            FROM cron_task_results
            WHERE executed_at >= ?1
            GROUP BY user_id, status
            "#,
        )
        .bind(since)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn store() -> RsiStore {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("in-memory sqlite");
        crate::migration::run_migrations(&pool).await.unwrap();
        RsiStore::new(pool)
    }

    fn report(id: &str, created_at: &str) -> RsiReport {
        RsiReport {
            id: id.to_string(),
            user_id: "u1".to_string(),
            role: "user".to_string(),
            period_start: "2026-11-14T00:00:00Z".to_string(),
            period_end: "2026-11-15T00:00:00Z".to_string(),
            summary: format!("summary-{}", id),
            output: format!("output-{}", id),
            status: "success".to_string(),
            created_at: created_at.to_string(),
        }
    }

    #[tokio::test]
    async fn insert_list_and_get_roundtrip() {
        let store = store().await;
        store.insert_report(&report("a", "2026-11-14T03:00:00Z")).await.unwrap();
        store.insert_report(&report("b", "2026-11-15T03:00:00Z")).await.unwrap();

        let list = store.list_reports("u1", 10).await.unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, "b", "newest first");
        assert_eq!(list[0].summary, "summary-b");

        let full = store.get_report("u1", "a").await.unwrap().unwrap();
        assert_eq!(full.output, "output-a");
        assert_eq!(store.latest_report_at("u1").await.unwrap().as_deref(), Some("2026-11-15T03:00:00Z"));

        // Another account cannot read it by id.
        assert!(store.get_report("u2", "a").await.unwrap().is_none());
        assert!(store.list_reports("u2", 10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn cron_task_stats_are_grouped_by_user_and_status() {
        let store = store().await;
        for (id, user, status, at) in [
            ("1", "u1", "success", "2026-11-15T01:00:00Z"),
            ("2", "u1", "success", "2026-11-15T02:00:00Z"),
            ("3", "u1", "error", "2026-11-15T03:00:00Z"),
            ("4", "u1", "success", "2026-11-01T00:00:00Z"),
        ] {
            sqlx::query(
                "INSERT INTO cron_task_results (id, cron_job_id, user_id, output, status, executed_at) \
                 VALUES (?, 'job', ?, '', ?, ?)",
            )
            .bind(id)
            .bind(user)
            .bind(status)
            .bind(at)
            .execute(&store.pool)
            .await
            .unwrap();
        }

        let stats = store.cron_task_stats_since("2026-11-14T00:00:00Z").await.unwrap();
        let get = |status: &str| {
            stats
                .iter()
                .find(|s| s.user_id == "u1" && s.status == status)
                .map(|s| s.count)
                .unwrap_or(0)
        };
        assert_eq!(get("success"), 2);
        assert_eq!(get("error"), 1);
    }
}
