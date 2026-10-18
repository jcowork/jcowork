//! SQLite persistence for cron jobs and their execution results.

use anyhow::Result;
use async_trait::async_trait;
use jcowork_cron::{CronStore, CronStoreJob, TaskResult, UserTaskResultEntry};
use sqlx::SqlitePool;

/// SQLite-backed implementation of [`CronStore`].
pub struct CronJobStore {
    pool: SqlitePool,
}

impl CronJobStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl CronStore for CronJobStore {
    async fn save_job(&self, job: CronStoreJob) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO cron_jobs (id, user_id, schedule, prompt, platform, enabled, last_run, created_at, name, model)
            VALUES (?1, ?2, ?3, ?4, 'api', ?5, ?6, ?7, ?8, ?9)
            ON CONFLICT(id) DO UPDATE SET
                schedule = excluded.schedule,
                prompt = excluded.prompt,
                enabled = excluded.enabled,
                last_run = excluded.last_run,
                name = excluded.name,
                model = excluded.model
            "#,
        )
        .bind(&job.id)
        .bind(&job.user_id)
        .bind(&job.schedule)
        .bind(&job.prompt)
        .bind(job.enabled)
        .bind(&job.last_run)
        .bind(&job.created_at)
        .bind(&job.name)
        .bind(&job.model)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn delete_job(&self, id: &str) -> Result<()> {
        sqlx::query("DELETE FROM cron_jobs WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn list_jobs(&self) -> Result<Vec<CronStoreJob>> {
        let rows: Vec<(
            String,
            String,
            String,
            String,
            bool,
            Option<String>,
            String,
            Option<String>,
            Option<String>,
        )> = sqlx::query_as(
            r#"
            SELECT id, user_id, schedule, prompt, enabled, last_run, created_at, name, model
            FROM cron_jobs
            "#,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(
                |(id, user_id, schedule, prompt, enabled, last_run, created_at, name, model)| {
                    CronStoreJob {
                        id,
                        user_id,
                        schedule,
                        prompt,
                        enabled,
                        last_run,
                        created_at,
                        name,
                        model,
                    }
                },
            )
            .collect())
    }

    async fn save_result(&self, result: TaskResult) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO cron_task_results (id, cron_job_id, user_id, output, status, executed_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            "#,
        )
        .bind(&result.id)
        .bind(&result.cron_job_id)
        .bind(&result.user_id)
        .bind(&result.output)
        .bind(&result.status)
        .bind(&result.executed_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn list_results(&self, cron_job_id: &str) -> Result<Vec<TaskResult>> {
        let rows: Vec<(String, String, String, String, String, String)> = sqlx::query_as(
            r#"
            SELECT id, cron_job_id, user_id, output, status, executed_at
            FROM cron_task_results
            WHERE cron_job_id = ?
            ORDER BY executed_at DESC
            LIMIT 100
            "#,
        )
        .bind(cron_job_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(
                |(id, cron_job_id, user_id, output, status, executed_at)| TaskResult {
                    id,
                    cron_job_id,
                    user_id,
                    output,
                    status,
                    executed_at,
                },
            )
            .collect())
    }

    async fn delete_result(&self, result_id: &str) -> Result<()> {
        sqlx::query("DELETE FROM cron_task_results WHERE id = ?")
            .bind(result_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn delete_results(&self, cron_job_id: &str) -> Result<()> {
        sqlx::query("DELETE FROM cron_task_results WHERE cron_job_id = ?")
            .bind(cron_job_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn list_user_results(
        &self,
        user_id: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<UserTaskResultEntry>> {
        // output is truncated in SQL: the aggregated feed only shows a summary
        // card, so shipping full task outputs would be pure waste.
        let rows: Vec<(
            String,
            String,
            String,
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            String,
        )> = sqlx::query_as(
            r#"
            SELECT r.id, r.cron_job_id, r.user_id, substr(r.output, 1, 400), r.status, r.executed_at,
                   j.name, j.prompt, r.summary
            FROM cron_task_results r
            LEFT JOIN cron_jobs j ON j.id = r.cron_job_id
            WHERE r.user_id = ?1
            ORDER BY r.executed_at DESC
            LIMIT ?2 OFFSET ?3
            "#,
        )
        .bind(user_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(
                |(id, cron_job_id, user_id, output, status, executed_at, job_name, job_prompt, summary)| {
                    UserTaskResultEntry {
                        id,
                        cron_job_id,
                        user_id,
                        output,
                        status,
                        executed_at,
                        job_name,
                        job_prompt,
                        summary,
                    }
                },
            )
            .collect())
    }

    async fn get_result(&self, result_id: &str) -> Result<Option<UserTaskResultEntry>> {
        let row: Option<(
            String,
            String,
            String,
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            String,
        )> = sqlx::query_as(
            r#"
            SELECT r.id, r.cron_job_id, r.user_id, r.output, r.status, r.executed_at,
                   j.name, j.prompt, r.summary
            FROM cron_task_results r
            LEFT JOIN cron_jobs j ON j.id = r.cron_job_id
            WHERE r.id = ?1
            "#,
        )
        .bind(result_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(
            |(id, cron_job_id, user_id, output, status, executed_at, job_name, job_prompt, summary)| {
                UserTaskResultEntry {
                    id,
                    cron_job_id,
                    user_id,
                    output,
                    status,
                    executed_at,
                    job_name,
                    job_prompt,
                    summary,
                }
            },
        ))
    }

    async fn set_result_summary(&self, result_id: &str, summary: &str) -> Result<()> {
        sqlx::query("UPDATE cron_task_results SET summary = ?2 WHERE id = ?1")
            .bind(result_id)
            .bind(summary)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}
