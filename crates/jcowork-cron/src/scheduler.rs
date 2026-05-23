//! Cron Scheduler - per-user scheduled job and reminder management.

use anyhow::Result;
use chrono::Utc;
use cron::Schedule;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

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
}

impl CronScheduler {
    pub fn new() -> Self {
        let (reminder_tx, _) = tokio::sync::broadcast::channel(256);
        Self {
            cron_jobs: Arc::new(Mutex::new(HashMap::new())),
            cron_handles: Arc::new(Mutex::new(HashMap::new())),
            reminders: Arc::new(Mutex::new(HashMap::new())),
            timers: Arc::new(Mutex::new(HashMap::new())),
            reminder_tx,
        }
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

        // Validate schedule first
        let schedule = Schedule::from_str(&schedule_expr)?;
        let next = schedule.upcoming(Utc).next()
            .ok_or_else(|| anyhow::anyhow!("No upcoming fire time for schedule"))?;

        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().naive_utc().to_string();
        let job = CronJob {
            id: id.clone(),
            user_id: user_id.to_string(),
            schedule: schedule_expr.to_string(),
            prompt: prompt.to_string(),
            enabled: true,
            last_run: None,
            created_at: now,
        };

        // Store job
        self.cron_jobs.lock().await.insert(id.clone(), job.clone());

        // Schedule the recurring task
        let job_id = id.clone();
        let cron_jobs = self.cron_jobs.clone();
        let reminder_tx = self.reminder_tx.clone();
        let user_id_clone = user_id.to_string();
        let prompt_clone = prompt.to_string();

        let handle = tokio::spawn(async move {
            loop {
                let schedule = match Schedule::from_str(&job.schedule) {
                    Ok(s) => s,
                    Err(_) => break,
                };
                let next = match schedule.upcoming(Utc).next() {
                    Some(n) => n,
                    None => break,
                };
                let delay = next.signed_duration_since(Utc::now());
                if delay.num_seconds() <= 0 {
                    // Sleep a bit to avoid busy loop on past times
                    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                    continue;
                }
                tokio::time::sleep(tokio::time::Duration::from_secs(delay.num_seconds() as u64)).await;

                // Mark last_run
                if let Some(j) = cron_jobs.lock().await.get_mut(&job_id) {
                    j.last_run = Some(Utc::now().naive_utc().to_string());
                }

                // Send as a reminder notification
                let reminder = Reminder {
                    id: uuid::Uuid::new_v4().to_string(),
                    user_id: user_id_clone.clone(),
                    fire_at: Utc::now().to_rfc3339(),
                    message: format!("[Cron] {}", prompt_clone),
                    triggered: true,
                };
                let _ = reminder_tx.send(reminder.clone());
                tracing::info!(id = %job_id, prompt = %prompt_clone, "Cron job triggered");
            }
        });

        self.cron_handles.lock().await.insert(id.clone(), handle);

        tracing::info!(id = %id, schedule = %schedule_expr, next = %next, "Cron job added");
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
        let schedule = Schedule::from_str(&schedule_expr)?;
        let next = schedule
            .upcoming(Utc)
            .next()
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
        Schedule::from_str(&schedule_expr)?;
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
