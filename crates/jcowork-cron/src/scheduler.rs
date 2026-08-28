//! Cron Scheduler - per-user scheduled job and reminder management.
//!
//! Schedule expressions are interpreted in the local timezone (default:
//! UTC+8 Beijing time, override minutes via `JCWORK_TZ_OFFSET`), so
//! "daily at 00:00" fires at local midnight rather than UTC midnight.

use anyhow::Result;
use async_trait::async_trait;
use chrono::{Datelike, FixedOffset, NaiveDate, NaiveDateTime, TimeZone, Timelike, Utc};
use cron::{Schedule, TimeUnitSpec};
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

/// Default local timezone offset in minutes (UTC+8, Beijing time).
const DEFAULT_TZ_OFFSET_MINUTES: i32 = 480;

/// Local timezone offset used to interpret schedule expressions.
fn local_offset() -> FixedOffset {
    let minutes = std::env::var("JCWORK_TZ_OFFSET")
        .ok()
        .and_then(|s| s.parse::<i32>().ok())
        .unwrap_or(DEFAULT_TZ_OFFSET_MINUTES);
    FixedOffset::east_opt(minutes * 60).unwrap_or_else(|| FixedOffset::east_opt(DEFAULT_TZ_OFFSET_MINUTES * 60).unwrap())
}

/// Parse a cron schedule expression (timezone-independent field matching).
fn parse_schedule(expr: &str) -> Result<Schedule, cron::error::Error> {
    Schedule::from_str(expr)
}

/// Compute the next fire time of a schedule, interpreting the fields in the
/// local timezone (the cron crate itself only supports UTC iteration).
///
/// Field matching is per-field AND, except day-of-month/day-of-week which
/// are OR-combined when both are restricted — mirroring standard cron.
fn next_local_fire(schedule: &Schedule, after: chrono::DateTime<Utc>) -> Option<chrono::DateTime<Utc>> {
    let offset = local_offset();
    let local_now = after.with_timezone(&offset);
    let mut year = local_now.year();

    // Search up to ~5 years ahead (covers Feb 29 schedules)
    for _ in 0..5 {
        let is_first_year = year == local_now.year();
        for month in schedule.months().iter() {
            if is_first_year && month < local_now.month() {
                continue;
            }
            let first_day = if is_first_year && month == local_now.month() {
                local_now.day()
            } else {
                1
            };
            let last_day = NaiveDate::from_ymd_opt(
                if month == 12 { year + 1 } else { year },
                if month == 12 { 1 } else { month + 1 },
                1,
            )?
            .pred_opt()?
            .day();

            for day in first_day..=last_day {
                let date = NaiveDate::from_ymd_opt(year, month, day)?;
                // cron crate DOW: 1=Sun..7=Sat; std cron: 0=Sun..6=Sat
                let dow = (date.weekday().num_days_from_sunday() + 1) as u32;

                let dom_restricted = schedule.days_of_month().iter().count() < 31;
                let dow_restricted = schedule.days_of_week().iter().count() < 7;
                let dom_ok = schedule.days_of_month().iter().any(|d| d == day);
                let dow_ok = schedule.days_of_week().iter().any(|d| d == dow);
                let day_ok = match (dom_restricted, dow_restricted) {
                    (true, true) => dom_ok || dow_ok,
                    (true, false) => dom_ok,
                    (false, true) => dow_ok,
                    _ => true,
                };
                if !day_ok {
                    continue;
                }

                let first_hour = if is_first_year && month == local_now.month() && day == local_now.day() {
                    local_now.hour()
                } else {
                    0
                };
                for hour in first_hour..=23 {
                    if !schedule.hours().iter().any(|h| h == hour) {
                        continue;
                    }
                    let first_min = if is_first_year && month == local_now.month() && day == local_now.day() && hour == local_now.hour() {
                        local_now.minute()
                    } else {
                        0
                    };
                    for minute in first_min..=59 {
                        if !schedule.minutes().iter().any(|m| m == minute) {
                            continue;
                        }
                        for second in schedule.seconds().iter() {
                            let naive = NaiveDateTime::new(
                                date,
                                chrono::NaiveTime::from_hms_opt(hour, minute, second)?,
                            );
                            let local_dt = offset.from_local_datetime(&naive).single()?;
                            if local_dt > local_now {
                                return Some(local_dt.with_timezone(&Utc));
                            }
                        }
                    }
                }
            }
        }
        year += 1;
    }
    None
}

/// A scheduled cron job.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CronJob {
    pub id: String,
    pub user_id: String,
    pub schedule: String,
    pub prompt: String,
    pub enabled: bool,
    pub last_run: Option<String>,
    pub created_at: String,
    /// Human-readable task name (UI-created tasks only).
    #[serde(default)]
    pub name: Option<String>,
    /// Model to use for execution, e.g. "deepseek:deepseek-chat".
    #[serde(default)]
    pub model: Option<String>,
}

/// Result of a periodic task execution.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TaskResult {
    pub id: String,
    pub cron_job_id: String,
    pub user_id: String,
    pub output: String,
    pub status: String, // "success" | "error"
    pub executed_at: String,
}

/// Flat representation of a persisted cron job (store layer interchange format).
#[derive(Debug, Clone)]
pub struct CronStoreJob {
    pub id: String,
    pub user_id: String,
    pub schedule: String,
    pub prompt: String,
    pub enabled: bool,
    pub last_run: Option<String>,
    pub created_at: String,
    pub name: Option<String>,
    pub model: Option<String>,
}

impl From<&CronJob> for CronStoreJob {
    fn from(j: &CronJob) -> Self {
        Self {
            id: j.id.clone(),
            user_id: j.user_id.clone(),
            schedule: j.schedule.clone(),
            prompt: j.prompt.clone(),
            enabled: j.enabled,
            last_run: j.last_run.clone(),
            created_at: j.created_at.clone(),
            name: j.name.clone(),
            model: j.model.clone(),
        }
    }
}

/// Persistence backend for cron jobs and their execution results.
/// Implementations must tolerate saving jobs/results for jobs they don't know.
#[async_trait]
pub trait CronStore: Send + Sync {
    async fn save_job(&self, job: CronStoreJob) -> Result<()>;
    async fn delete_job(&self, id: &str) -> Result<()>;
    async fn list_jobs(&self) -> Result<Vec<CronStoreJob>>;
    async fn save_result(&self, result: TaskResult) -> Result<()>;
    async fn list_results(&self, cron_job_id: &str) -> Result<Vec<TaskResult>>;
    async fn delete_results(&self, cron_job_id: &str) -> Result<()>;
}

/// A one-time reminder.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Reminder {
    pub id: String,
    pub user_id: String,
    /// ISO 8601 datetime string (e.g., "2026-05-15T11:41:00+08:00")
    pub fire_at: String,
    /// The reminder message to send to the user.
    pub message: String,
    /// Whether the reminder has been triggered.
    pub triggered: bool,
    /// Optional action to execute when the reminder fires (e.g., "search 俄乌战争最新进展")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    /// Associated cron job ID (for periodic task execution).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cron_job_id: Option<String>,
    /// Model to use for execution (for periodic tasks).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Original prompt text (for cron job execution).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
}

/// Manages per-user cron jobs and reminders.
pub struct CronScheduler {
    /// Cron job store: job_id -> CronJob
    cron_jobs: Arc<Mutex<HashMap<String, CronJob>>>,
    /// Running cron task handles: job_id -> JoinHandle
    cron_handles: Arc<Mutex<HashMap<String, JoinHandle<()>>>>,
    /// In-memory reminder store: reminder_id -> Reminder
    reminders: Arc<Mutex<HashMap<String, Reminder>>>,
    /// Pending reminder timers: reminder_id -> JoinHandle
    timers: Arc<Mutex<HashMap<String, JoinHandle<()>>>>,
    /// Channel to send reminder notifications.
    reminder_tx: tokio::sync::broadcast::Sender<Reminder>,
    /// Per-task execution results: cron_job_id -> Vec<TaskResult>
    task_results: Arc<Mutex<HashMap<String, Vec<TaskResult>>>>,
    /// Optional persistence backend (must be set before Arc-wrapping the scheduler).
    store: Option<Arc<dyn CronStore>>,
    /// Shared clones used by spawned job loops (stable addresses).
    jobs_ref: Arc<Mutex<HashMap<String, CronJob>>>,
    handles_ref: Arc<Mutex<HashMap<String, JoinHandle<()>>>>,
}

impl CronScheduler {
    pub fn new() -> Self {
        let (reminder_tx, _) = tokio::sync::broadcast::channel(256);
        let cron_jobs = Arc::new(Mutex::new(HashMap::new()));
        let cron_handles = Arc::new(Mutex::new(HashMap::new()));
        Self {
            jobs_ref: cron_jobs.clone(),
            handles_ref: cron_handles.clone(),
            cron_jobs,
            cron_handles,
            reminders: Arc::new(Mutex::new(HashMap::new())),
            timers: Arc::new(Mutex::new(HashMap::new())),
            reminder_tx,
            task_results: Arc::new(Mutex::new(HashMap::new())),
            store: None,
        }
    }

    /// Attach a persistence backend. Must be called before the scheduler is
    /// wrapped in `Arc` and shared. Call `restore` afterwards to load and
    /// re-schedule persisted jobs.
    pub fn with_store(&mut self, store: Arc<dyn CronStore>) {
        self.store = Some(store);
    }

    /// Load persisted jobs from the store and re-schedule them.
    /// Jobs already present in memory are skipped.
    pub async fn restore(&self) {
        let Some(store) = self.store.clone() else {
            return;
        };
        let jobs = match store.list_jobs().await {
            Ok(jobs) => jobs,
            Err(e) => {
                tracing::warn!(error = %e, "Cron restore: failed to load persisted jobs");
                return;
            }
        };
        let mut restored = 0;
        for sj in jobs {
            if !sj.enabled {
                continue;
            }
            let already = self.cron_jobs.lock().await.contains_key(&sj.id);
            if already {
                continue;
            }
            let job = CronJob {
                id: sj.id.clone(),
                user_id: sj.user_id.clone(),
                schedule: sj.schedule.clone(),
                prompt: sj.prompt.clone(),
                enabled: sj.enabled,
                last_run: sj.last_run.clone(),
                created_at: sj.created_at.clone(),
                name: sj.name.clone(),
                model: sj.model.clone(),
            };
            if let Err(e) = self.spawn_job_loop(job).await {
                tracing::warn!(error = %e, id = %sj.id, "Cron restore: failed to schedule job");
            } else {
                restored += 1;
            }
        }
        tracing::info!(restored, "Cron scheduler restored persisted jobs");
    }

    /// Spawn the recurring trigger loop for a job and register it in memory.
    async fn spawn_job_loop(&self, job: CronJob) -> Result<()> {
        // Validate upfront so callers get a meaningful error.
        let schedule = parse_schedule(&job.schedule)?;
        let next = next_local_fire(&schedule, Utc::now())
            .ok_or_else(|| anyhow::anyhow!("No upcoming fire time for schedule"))?;

        let job_id = job.id.clone();
        let user_id = job.user_id.clone();
        let prompt = job.prompt.clone();
        let model = job.model.clone();
        let schedule_expr = job.schedule.clone();
        let cron_jobs = self.jobs_ref.clone();
        let reminder_tx = self.reminder_tx.clone();
        let store = self.store.clone();

        let handle = tokio::spawn(async move {
            loop {
                let schedule = match parse_schedule(&schedule_expr) {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::error!(error = %e, id = %job_id, "Cron loop: invalid schedule, stopping");
                        break;
                    }
                };
                let next = match next_local_fire(&schedule, Utc::now()) {
                    Some(n) => n,
                    None => break,
                };
                let delay = next.signed_duration_since(Utc::now());
                if delay.num_seconds() <= 0 {
                    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                    continue;
                }
                tokio::time::sleep(tokio::time::Duration::from_secs(delay.num_seconds() as u64)).await;

                let last_run = Utc::now().naive_utc().to_string();
                {
                    let mut jobs = cron_jobs.lock().await;
                    match jobs.get_mut(&job_id) {
                        Some(j) => j.last_run = Some(last_run.clone()),
                        // Job was removed while we were sleeping.
                        None => break,
                    }
                }
                // Persist last_run update
                if let Some(store) = &store {
                    let jobs = cron_jobs.lock().await;
                    if let Some(j) = jobs.get(&job_id) {
                        if let Err(e) = store.save_job(CronStoreJob::from(j)).await {
                            tracing::warn!(error = %e, id = %job_id, "Cron loop: failed to persist last_run");
                        }
                    }
                }

                let reminder = Reminder {
                    id: uuid::Uuid::new_v4().to_string(),
                    user_id: user_id.clone(),
                    fire_at: Utc::now().to_rfc3339(),
                    message: format!("[Cron] {}", prompt),
                    triggered: true,
                    action: None,
                    cron_job_id: Some(job_id.clone()),
                    model: model.clone(),
                    prompt: Some(prompt.clone()),
                };
                let _ = reminder_tx.send(reminder.clone());
                tracing::info!(id = %job_id, prompt = %prompt, "Cron job triggered");
            }
        });

        // Register in memory maps
        self.cron_jobs.lock().await.insert(job.id.clone(), job.clone());
        self.cron_handles.lock().await.insert(job.id.clone(), handle);
        tracing::info!(id = %job.id, schedule = %job.schedule, next = %next, "Cron job scheduled");
        Ok(())
    }

    /// Add a periodic task with name, model, and schedule details.
    /// Returns the job ID.
    pub async fn add_periodic_task(
        &self,
        user_id: &str,
        name: &str,
        prompt: &str,
        model: &str,
        schedule_expr: &str,
    ) -> Result<String> {
        // Auto-convert 5-field cron to 7-field
        let schedule_expr = if schedule_expr.split_whitespace().count() == 5 {
            Self::convert_5field_to_7field(schedule_expr)
        } else {
            schedule_expr.to_string()
        };

        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().naive_utc().to_string();
        let job = CronJob {
            id: id.clone(),
            user_id: user_id.to_string(),
            schedule: schedule_expr.clone(),
            prompt: prompt.to_string(),
            enabled: true,
            last_run: None,
            created_at: now,
            name: Some(name.to_string()),
            model: Some(model.to_string()),
        };

        // Validate + spawn the recurring loop (also registers in memory)
        self.spawn_job_loop(job.clone()).await?;

        // Persist
        if let Some(store) = &self.store {
            if let Err(e) = store.save_job(CronStoreJob::from(&job)).await {
                tracing::warn!(error = %e, id = %id, "Failed to persist periodic task");
            }
        }

        tracing::info!(id = %id, name = %name, schedule = %schedule_expr, "Periodic task added");
        Ok(id)
    }

    /// Store a task execution result (memory + persistence).
    pub async fn store_task_result(&self, result: TaskResult) {
        if let Some(store) = &self.store {
            if let Err(e) = store.save_result(result.clone()).await {
                tracing::warn!(error = %e, cron_job_id = %result.cron_job_id, "Failed to persist task result");
            }
        }
        let mut results = self.task_results.lock().await;
        results
            .entry(result.cron_job_id.clone())
            .or_insert_with(Vec::new)
            .push(result);
    }

    /// List execution results for a specific cron job (most recent first).
    /// Merges persisted results (if a store is attached) with the in-memory cache.
    pub async fn list_task_results(&self, cron_job_id: &str) -> Vec<TaskResult> {
        let mut items: Vec<TaskResult> = match &self.store {
            Some(store) => match store.list_results(cron_job_id).await {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(error = %e, cron_job_id = %cron_job_id, "Failed to load persisted results");
                    self.task_results.lock().await.get(cron_job_id).cloned().unwrap_or_default()
                }
            },
            None => self.task_results.lock().await.get(cron_job_id).cloned().unwrap_or_default(),
        };
        items.sort_by(|a, b| b.executed_at.cmp(&a.executed_at)); // most recent first
        items
    }

    /// Subscribe to reminder notifications.
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<Reminder> {
        self.reminder_tx.subscribe()
    }

    /// Add a one-time reminder.
    /// `fire_at` is an ISO 8601 datetime string.
    /// Returns the reminder ID.
    pub async fn add_reminder(
        &self,
        user_id: &str,
        fire_at: &str,
        message: &str,
        action: Option<&str>,
    ) -> Result<String> {
        let fire_time = chrono::DateTime::parse_from_rfc3339(fire_at)
            .map_err(|e| anyhow::anyhow!("Invalid datetime format '{}': {}. Expected ISO 8601 (e.g., 2026-05-15T11:41:00+08:00)", fire_at, e))?;

        let id = uuid::Uuid::new_v4().to_string();
        let reminder = Reminder {
            id: id.clone(),
            user_id: user_id.to_string(),
            fire_at: fire_at.to_string(),
            message: message.to_string(),
            triggered: false,
            action: action.map(|s| s.to_string()),
            cron_job_id: None,
            model: None,
            prompt: None,
        };

        // Calculate delay
        let now = Utc::now();
        let fire_utc = fire_time.with_timezone(&Utc);
        let delay = fire_utc.signed_duration_since(now);

        if delay.num_seconds() <= 0 {
            return Err(anyhow::anyhow!(
                "Reminder time is in the past: {}",
                fire_at
            ));
        }

        // Store reminder
        self.reminders.lock().await.insert(id.clone(), reminder.clone());

        // Schedule the timer
        let reminder_id = id.clone();
        let reminders = self.reminders.clone();
        let reminder_tx = self.reminder_tx.clone();
        let delay_duration = tokio::time::Duration::from_secs(delay.num_seconds().max(0) as u64);

        let handle = tokio::spawn(async move {
            tokio::time::sleep(delay_duration).await;
            // Mark as triggered
            if let Some(r) = reminders.lock().await.get_mut(&reminder_id) {
                r.triggered = true;
            }
            // Notify (ignore send error — no subscribers)
            let _ = reminder_tx.send(reminder.clone());
            tracing::info!(id = %reminder.id, message = %reminder.message, "Reminder triggered");
        });

        self.timers.lock().await.insert(id.clone(), handle);

        tracing::info!(id = %id, fire_at = %fire_at, message = %message, "Reminder added");
        Ok(id)
    }

    /// List all reminders for a user.
    pub async fn list_reminders(&self, user_id: &str) -> Vec<Reminder> {
        self.reminders
            .lock()
            .await
            .values()
            .filter(|r| r.user_id == user_id && !r.triggered)
            .cloned()
            .collect()
    }

    /// Remove a reminder by ID.
    pub async fn remove_reminder(&self, id: &str) -> Result<()> {
        // Cancel the timer if it exists
        if let Some(handle) = self.timers.lock().await.remove(id) {
            handle.abort();
        }
        self.reminders.lock().await.remove(id);
        Ok(())
    }

    /// Add a cron job (recurring schedule).
    /// Returns the job ID.
    /// Supports both 5-field (min hour dom month dow) and 7-field (sec min hour dom month dow year) cron syntax.
    /// If a 5-field expression is given, it is auto-converted to 7-field by prepending "0 ".
    pub async fn add_cron_job(
        &self,
        user_id: &str,
        schedule_expr: &str,
        prompt: &str,
    ) -> Result<String> {
        // Auto-convert 5-field cron to 7-field (add seconds field + remap DOW)
        let schedule_expr = if schedule_expr.split_whitespace().count() == 5 {
            Self::convert_5field_to_7field(schedule_expr)
        } else {
            schedule_expr.to_string()
        };

        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().naive_utc().to_string();
        let job = CronJob {
            id: id.clone(),
            user_id: user_id.to_string(),
            schedule: schedule_expr.clone(),
            prompt: prompt.to_string(),
            enabled: true,
            last_run: None,
            created_at: now,
            name: None,
            model: None,
        };

        // Validate + spawn the recurring loop (also registers in memory)
        self.spawn_job_loop(job.clone()).await?;

        // Persist
        if let Some(store) = &self.store {
            if let Err(e) = store.save_job(CronStoreJob::from(&job)).await {
                tracing::warn!(error = %e, id = %id, "Failed to persist cron job");
            }
        }

        tracing::info!(id = %id, schedule = %schedule_expr, "Cron job added");
        Ok(id)
    }

    /// List all cron jobs for a user.
    pub async fn list_cron_jobs(&self, user_id: &str) -> Vec<CronJob> {
        self.cron_jobs
            .lock()
            .await
            .values()
            .filter(|j| j.user_id == user_id && j.enabled)
            .cloned()
            .collect()
    }

    /// Remove a cron job by ID.
    pub async fn remove_cron_job(&self, id: &str) -> Result<()> {
        // Cancel the running task if exists
        if let Some(handle) = self.cron_handles.lock().await.remove(id) {
            handle.abort();
        }
        self.cron_jobs.lock().await.remove(id);
        // Remove from persistence (job + its execution results)
        if let Some(store) = &self.store {
            if let Err(e) = store.delete_job(id).await {
                tracing::warn!(error = %e, id = %id, "Failed to delete persisted cron job");
            }
            if let Err(e) = store.delete_results(id).await {
                tracing::warn!(error = %e, id = %id, "Failed to delete persisted task results");
            }
        }
        self.task_results.lock().await.remove(id);
        Ok(())
    }

    /// Parse a cron schedule expression and get the next fire time.
    /// Supports both 5-field and 7-field cron syntax.
    pub fn next_fire_time(schedule_expr: &str) -> Result<chrono::DateTime<Utc>> {
        let schedule_expr = if schedule_expr.split_whitespace().count() == 5 {
            Self::convert_5field_to_7field(schedule_expr)
        } else {
            schedule_expr.to_string()
        };
        let schedule = parse_schedule(&schedule_expr)?;
        let next = next_local_fire(&schedule, Utc::now())
            .ok_or_else(|| anyhow::anyhow!("No upcoming fire time for schedule"))?;
        Ok(next)
    }

    /// Validate a cron schedule expression.
    /// Supports both 5-field and 7-field cron syntax.
    pub fn validate_schedule(schedule_expr: &str) -> Result<()> {
        let schedule_expr = if schedule_expr.split_whitespace().count() == 5 {
            Self::convert_5field_to_7field(schedule_expr)
        } else {
            schedule_expr.to_string()
        };
        parse_schedule(&schedule_expr)?;
        Ok(())
    }

    /// Convert a 5-field cron expression to 7-field format compatible with the `cron` crate.
    ///
    /// The `cron` crate uses 1=Sun, 2=Mon, ..., 7=Sat (where both 0 and 7 = Sunday),
    /// but standard 5-field cron uses 0=Sun, 1=Mon, ..., 6=Sat.
    /// When converting from 5-field to 7-field, the DOW field must be remapped:
    /// standard 0 (Sun) -> crate 1, standard 6 (Sat) -> crate 7.
    fn convert_5field_to_7field(expr: &str) -> String {
        let parts: Vec<&str> = expr.split_whitespace().collect();
        if parts.len() != 5 {
            return expr.to_string();
        }
        // parts: min hour dom month dow
        let remapped_dow = remap_dow(parts[4]);
        format!("0 {} {} {} {} {} *", parts[0], parts[1], parts[2], parts[3], remapped_dow)
    }
}

impl Default for CronScheduler {
    fn default() -> Self {
        Self::new()
    }
}

/// Remap DOW field from standard cron (0=Sun..6=Sat) to `cron` crate (1=Sun..7=Sat).
fn remap_dow(dow: &str) -> String {
    if dow == "*" {
        return "*".to_string();
    }

    // Handle step expressions: e.g. "1-5/2"
    let (range_part, step_part) = if let Some(slash_pos) = dow.find('/') {
        let (r, s) = dow.split_at(slash_pos);
        (r, Some(s))
    } else {
        (dow, None)
    };

    // Handle comma-separated items: e.g. "1,3,5"
    let items: Vec<&str> = range_part.split(',').collect();
    let remapped: Vec<String> = items.iter().map(|item| remap_dow_item(item.trim())).collect();

    let result = remapped.join(",");
    match step_part {
        Some(s) => format!("{}{}", result, s),
        None => result,
    }
}

/// Remap a single DOW item (number or range like "1-5").
fn remap_dow_item(item: &str) -> String {
    if item.contains('-') {
        let parts: Vec<&str> = item.split('-').collect();
        if parts.len() == 2 {
            let start = remap_dow_value(parts[0].trim());
            let end = remap_dow_value(parts[1].trim());
            return format!("{}-{}", start, end);
        }
    }
    remap_dow_value(item)
}

/// Remap a single DOW number: standard 0-6 -> crate 1-7.
fn remap_dow_value(val: &str) -> String {
    if let Ok(n) = val.parse::<u32>() {
        // Standard cron: 0=Sun, 1=Mon, ..., 6=Sat
        // cron crate:   1=Sun, 2=Mon, ..., 7=Sat
        (n + 1).to_string()
    } else {
        // Named days (SUN, MON, etc.) -- leave as-is, the cron crate supports them
        val.to_uppercase()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_next_local_fire_daily_midnight() {
        // "0 0 0 * * *" = daily at 00:00:00 local time (UTC+8 by default).
        // Must fire at local midnight => 16:00 UTC the previous day, NOT 00:00 UTC.
        let schedule = parse_schedule("0 0 0 * * *").unwrap();
        // 2026-08-29 02:00 Beijing = 2026-08-28 18:00 UTC
        let after = chrono::DateTime::parse_from_rfc3339("2026-08-28T18:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let next = next_local_fire(&schedule, after).expect("should find next fire");
        // Expected: 2026-08-30 00:00 Beijing = 2026-08-29 16:00 UTC
        assert_eq!(next.to_rfc3339(), "2026-08-29T16:00:00+00:00");
    }

    #[test]
    fn test_next_local_fire_specific_time() {
        // Daily at 09:30 local => 01:30 UTC
        let schedule = parse_schedule("0 30 9 * * *").unwrap();
        let after = chrono::DateTime::parse_from_rfc3339("2026-08-29T02:00:00+08:00")
            .unwrap()
            .with_timezone(&Utc);
        let next = next_local_fire(&schedule, after).unwrap();
        assert_eq!(next.to_rfc3339(), "2026-08-29T01:30:00+00:00");

        // After today's 09:30 local, next fire is tomorrow
        let after_late = chrono::DateTime::parse_from_rfc3339("2026-08-29T10:00:00+08:00")
            .unwrap()
            .with_timezone(&Utc);
        let next2 = next_local_fire(&schedule, after_late).unwrap();
        assert_eq!(next2.to_rfc3339(), "2026-08-30T01:30:00+00:00");
    }

    #[test]
    fn test_cron_dow_mapping() {
        // Standard cron: 0=Sun, 1=Mon, ..., 6=Sat
        // cron crate:  1=Sun, 2=Mon, ..., 7=Sat
        // After remap, standard "6" (Sat) should become "7" (Sat in crate)
        let base = chrono::DateTime::parse_from_rfc3339("2026-05-17T10:00:00+00:00")
            .unwrap()
            .with_timezone(&Utc);

        // Test the convert_5field_to_7field function
        // "50 11 * * 6" (standard: Sat 11:50) should convert correctly
        let converted = CronScheduler::convert_5field_to_7field("50 11 * * 6");
        assert_eq!(converted, "0 50 11 * * 7 *");

        let schedule = Schedule::from_str(&converted).unwrap();
        let next = schedule.after(&base).next().unwrap();
        assert_eq!(next.format("%A").to_string(), "Saturday");
        assert_eq!(next.format("%Y-%m-%d").to_string(), "2026-05-23");

        // "0 11 * * 0" (standard: Sun 11:00) -> dow 0->1
        let converted_sun = CronScheduler::convert_5field_to_7field("0 11 * * 0");
        assert_eq!(converted_sun, "0 0 11 * * 1 *");

        let sched_sun = Schedule::from_str(&converted_sun).unwrap();
        let next_sun = sched_sun.after(&base).next().unwrap();
        assert_eq!(next_sun.format("%A").to_string(), "Sunday");

        // "0 11 * * 1-5" (Mon-Fri) -> dow 1-5 -> 2-6
        let converted_weekday = CronScheduler::convert_5field_to_7field("0 11 * * 1-5");
        assert_eq!(converted_weekday, "0 0 11 * * 2-6 *");

        // "*" should stay as "*"
        let converted_any = CronScheduler::convert_5field_to_7field("0 11 * * *");
        assert_eq!(converted_any, "0 0 11 * * * *");
    }

    #[test]
    fn test_remap_dow() {
        assert_eq!(remap_dow("*"), "*");
        assert_eq!(remap_dow("0"), "1"); // Sun
        assert_eq!(remap_dow("6"), "7"); // Sat
        assert_eq!(remap_dow("1-5"), "2-6"); // Mon-Fri
        assert_eq!(remap_dow("1,3,5"), "2,4,6"); // Mon,Wed,Fri
        assert_eq!(remap_dow("1-5/2"), "2-6/2"); // Mon-Fri step 2
        assert_eq!(remap_dow("SUN"), "SUN"); // Named day
        assert_eq!(remap_dow("mon"), "MON"); // Named day lower
    }
}
