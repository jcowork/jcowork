//! Reminders and periodic task (cron job) endpoints.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::Deserialize;

use super::{AppState, AuthUser};
use jcowork_cron::TaskResult;

pub(crate) async fn list_reminders(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
) -> impl IntoResponse {
    let reminders = state.cron_scheduler.list_reminders(&auth_user.user_id).await;
    (StatusCode::OK, Json(reminders))
}

pub(crate) async fn remove_reminder(
    State(state): State<AppState>,
    _auth: axum::Extension<AuthUser>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.cron_scheduler.remove_reminder(&id).await {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({"status": "removed"}))),
        Err(e) => (StatusCode::NOT_FOUND, Json(serde_json::json!({"error": e.to_string()}))),
    }
}

pub(crate) async fn list_cron_jobs(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
) -> impl IntoResponse {
    let jobs = state.cron_scheduler.list_cron_jobs(&auth_user.user_id).await;
    (StatusCode::OK, Json(jobs))
}

pub(crate) async fn remove_cron_job(
    State(state): State<AppState>,
    _auth: axum::Extension<AuthUser>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.cron_scheduler.remove_cron_job(&id).await {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({"status": "removed"}))),
        Err(e) => (StatusCode::NOT_FOUND, Json(serde_json::json!({"error": e.to_string()}))),
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreateCronJobRequest {
    pub name: String,
    pub prompt: String,
    pub model: String,
    pub frequency: String,         // "hourly" | "daily" | "weekly" | "monthly" | "yearly"
    pub second: Option<u32>,       // specific second (0-59)
    pub minute: Option<u32>,       // for hourly: specific minute
    pub hour: Option<u32>,         // for daily/weekly/monthly/yearly: specific hour
    pub day: Option<u32>,          // for monthly/yearly: specific day (1-28)
    pub month: Option<u32>,        // for yearly: specific month (1-12)
    pub days_of_week: Option<Vec<u32>>, // for weekly: list of days (0=Sun, 1=Mon, ..., 6=Sat)
}

/// Convert frequency + time parameters to a 6-field cron expression (second minute hour dom month dow).
/// Returns Err if frequency is invalid.
fn build_cron_expression(
    frequency: &str,
    second: Option<u32>,
    minute: Option<u32>,
    hour: Option<u32>,
    day: Option<u32>,
    month: Option<u32>,
    days_of_week: Option<Vec<u32>>,
) -> Result<String, String> {
    let sec = second.unwrap_or(0);
    if sec > 59 {
        return Err(format!("Invalid second {} (must be 0-59)", sec));
    }
    match frequency {
        "hourly" => {
            let minute = minute.unwrap_or(0);
            if minute > 59 {
                return Err(format!("Invalid minute {} for hourly (must be 0-59)", minute));
            }
            Ok(format!("{} {} * * * *", sec, minute))
        }
        "daily" => {
            let minute = minute.unwrap_or(0);
            let hour = hour.unwrap_or(9);
            if minute > 59 {
                return Err(format!("Invalid minute {} for daily (must be 0-59)", minute));
            }
            if hour > 23 {
                return Err(format!("Invalid hour {} for daily (must be 0-23)", hour));
            }
            Ok(format!("{} {} {} * * *", sec, minute, hour))
        }
        "weekly" => {
            let minute = minute.unwrap_or(0);
            let hour = hour.unwrap_or(9);
            let dows = days_of_week.unwrap_or_else(|| vec![1]); // Default: Monday
            if minute > 59 {
                return Err(format!("Invalid minute {} for weekly (must be 0-59)", minute));
            }
            if hour > 23 {
                return Err(format!("Invalid hour {} for weekly (must be 0-23)", hour));
            }
            if dows.is_empty() {
                return Err("days_of_week must not be empty for weekly".to_string());
            }
            // Validate and sort days
            let sorted_dows: Vec<u32> = dows.into_iter().collect::<std::collections::BTreeSet<_>>().into_iter().collect();
            for &dow in &sorted_dows {
                if dow > 6 {
                    return Err(format!("Invalid day_of_week {} for weekly (must be 0-6)", dow));
                }
            }
            let dow_str = sorted_dows.iter().map(|d| d.to_string()).collect::<Vec<_>>().join(",");
            Ok(format!("{} {} {} * * {}", sec, minute, hour, dow_str))
        }
        "monthly" => {
            let minute = minute.unwrap_or(0);
            let hour = hour.unwrap_or(9);
            let day = day.unwrap_or(1).min(28);
            if minute > 59 {
                return Err(format!("Invalid minute {} for monthly (must be 0-59)", minute));
            }
            if hour > 23 {
                return Err(format!("Invalid hour {} for monthly (must be 0-23)", hour));
            }
            Ok(format!("{} {} {} {} * *", sec, minute, hour, day))
        }
        "yearly" => {
            let minute = minute.unwrap_or(0);
            let hour = hour.unwrap_or(9);
            let day = day.unwrap_or(1).min(28);
            let month = month.unwrap_or(1).max(1).min(12);
            if minute > 59 {
                return Err(format!("Invalid minute {} for yearly (must be 0-59)", minute));
            }
            if hour > 23 {
                return Err(format!("Invalid hour {} for yearly (must be 0-23)", hour));
            }
            Ok(format!("{} {} {} {} {} *", sec, minute, hour, day, month))
        }
        other => Err(format!("Invalid frequency '{}'. Use: hourly, daily, weekly, monthly, yearly", other)),
    }
}

/// POST /api/cron-jobs - create a periodic task with name, model, and frequency.
pub(crate) async fn create_cron_job(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Json(req): Json<CreateCronJobRequest>,
) -> impl IntoResponse {
    // Convert frequency + time to cron expression
    let schedule_expr = match build_cron_expression(&req.frequency, req.second, req.minute, req.hour, req.day, req.month, req.days_of_week) {
        Ok(expr) => expr,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": e})),
            );
        }
    };

    match state.cron_scheduler.add_periodic_task(
        &auth_user.user_id,
        &req.name,
        &req.prompt,
        &req.model,
        &schedule_expr,
    ).await {
        Ok(id) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "id": id,
                "schedule": schedule_expr,
                "status": "created",
            })),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ),
    }
}

/// GET /api/cron-jobs/{id}/results - get execution results for a task.
pub(crate) async fn get_cron_job_results(
    State(state): State<AppState>,
    _auth: axum::Extension<AuthUser>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let results = state.cron_scheduler.list_task_results(&id).await;
    (StatusCode::OK, Json(results))
}

#[derive(Debug, Deserialize)]
pub(crate) struct StoreCronJobResultRequest {
    pub output: String,
    pub status: String,
}

/// POST /api/cron-jobs/{id}/results - store an execution result.
pub(crate) async fn store_cron_job_result(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Path(id): Path<String>,
    Json(req): Json<StoreCronJobResultRequest>,
) -> impl IntoResponse {
    let result = TaskResult {
        id: uuid::Uuid::new_v4().to_string(),
        cron_job_id: id.clone(),
        user_id: auth_user.user_id,
        output: req.output,
        status: req.status,
        executed_at: chrono::Utc::now().to_rfc3339(),
    };
    state.cron_scheduler.store_task_result(result).await;
    (StatusCode::OK, Json(serde_json::json!({"status": "stored"})))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Helper function for cleaner tests
    fn build(freq: &str, sec: Option<u32>, min: Option<u32>, hr: Option<u32>, day: Option<u32>, month: Option<u32>, dows: Option<Vec<u32>>) -> Result<String, String> {
        build_cron_expression(freq, sec, min, hr, day, month, dows)
    }

    // ========== build_cron_expression tests ==========

    // --- Hourly tests ---
    #[test]
    fn test_hourly_default() {
        assert_eq!(build("hourly", None, None, None, None, None, None).unwrap(), "0 0 * * * *");
    }

    #[test]
    fn test_hourly_specific_second() {
        assert_eq!(build("hourly", Some(30), None, None, None, None, None).unwrap(), "30 0 * * * *");
    }

    #[test]
    fn test_hourly_specific_minute() {
        assert_eq!(build("hourly", None, Some(30), None, None, None, None).unwrap(), "0 30 * * * *");
    }

    #[test]
    fn test_hourly_second_and_minute() {
        assert_eq!(build("hourly", Some(15), Some(30), None, None, None, None).unwrap(), "15 30 * * * *");
    }

    #[test]
    fn test_hourly_minute_59() {
        assert_eq!(build("hourly", None, Some(59), None, None, None, None).unwrap(), "0 59 * * * *");
    }

    #[test]
    fn test_hourly_invalid_second() {
        let result = build("hourly", Some(60), None, None, None, None, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Invalid second"));
    }

    #[test]
    fn test_hourly_invalid_minute() {
        let result = build("hourly", None, Some(60), None, None, None, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Invalid minute"));
    }

    #[test]
    fn test_hourly_ignores_other_params() {
        assert_eq!(build("hourly", None, Some(15), Some(10), Some(5), None, Some(vec![3])).unwrap(), "0 15 * * * *");
    }

    // --- Daily tests ---
    #[test]
    fn test_daily_default() {
        assert_eq!(build("daily", None, None, None, None, None, None).unwrap(), "0 0 9 * * *");
    }

    #[test]
    fn test_daily_specific_time() {
        assert_eq!(build("daily", None, Some(30), Some(15), None, None, None).unwrap(), "0 30 15 * * *");
    }

    #[test]
    fn test_daily_with_second() {
        assert_eq!(build("daily", Some(45), Some(30), Some(15), None, None, None).unwrap(), "45 30 15 * * *");
    }

    #[test]
    fn test_daily_midnight() {
        assert_eq!(build("daily", None, Some(0), Some(0), None, None, None).unwrap(), "0 0 0 * * *");
    }

    #[test]
    fn test_daily_end_of_day() {
        assert_eq!(build("daily", None, Some(59), Some(23), None, None, None).unwrap(), "0 59 23 * * *");
    }

    #[test]
    fn test_daily_invalid_minute() {
        let result = build("daily", None, Some(60), Some(10), None, None, None);
        assert!(result.is_err());
    }

    #[test]
    fn test_daily_invalid_hour() {
        let result = build("daily", None, Some(30), Some(24), None, None, None);
        assert!(result.is_err());
    }

    #[test]
    fn test_daily_ignores_day_and_dows() {
        assert_eq!(build("daily", None, Some(0), Some(12), Some(15), None, Some(vec![3])).unwrap(), "0 0 12 * * *");
    }

    // --- Weekly tests (multi-select) ---
    #[test]
    fn test_weekly_default() {
        // Default: Monday at 9:00
        assert_eq!(build("weekly", None, None, None, None, None, None).unwrap(), "0 0 9 * * 1");
    }

    #[test]
    fn test_weekly_single_day_sunday() {
        assert_eq!(build("weekly", None, Some(0), Some(10), None, None, Some(vec![0])).unwrap(), "0 0 10 * * 0");
    }

    #[test]
    fn test_weekly_single_day_saturday() {
        assert_eq!(build("weekly", None, Some(30), Some(14), None, None, Some(vec![6])).unwrap(), "0 30 14 * * 6");
    }

    #[test]
    fn test_weekly_with_second() {
        assert_eq!(build("weekly", Some(30), Some(0), Some(9), None, None, Some(vec![1])).unwrap(), "30 0 9 * * 1");
    }

    #[test]
    fn test_weekly_multiple_days() {
        // Mon, Wed, Fri
        assert_eq!(build("weekly", None, Some(0), Some(9), None, None, Some(vec![1, 3, 5])).unwrap(), "0 0 9 * * 1,3,5");
    }

    #[test]
    fn test_weekly_weekdays_equivalent() {
        // Mon-Fri (like old weekdays)
        assert_eq!(build("weekly", None, Some(0), Some(9), None, None, Some(vec![1, 2, 3, 4, 5])).unwrap(), "0 0 9 * * 1,2,3,4,5");
    }

    #[test]
    fn test_weekly_weekends_equivalent() {
        // Sat, Sun (like old weekends)
        assert_eq!(build("weekly", None, Some(0), Some(10), None, None, Some(vec![0, 6])).unwrap(), "0 0 10 * * 0,6");
    }

    #[test]
    fn test_weekly_all_days() {
        // Every day
        assert_eq!(build("weekly", None, Some(0), Some(9), None, None, Some(vec![0, 1, 2, 3, 4, 5, 6])).unwrap(), "0 0 9 * * 0,1,2,3,4,5,6");
    }

    #[test]
    fn test_weekly_duplicate_days_deduped() {
        // Duplicates should be removed
        assert_eq!(build("weekly", None, Some(0), Some(9), None, None, Some(vec![1, 1, 3, 3])).unwrap(), "0 0 9 * * 1,3");
    }

    #[test]
    fn test_weekly_unsorted_days_sorted() {
        // Days should be sorted
        assert_eq!(build("weekly", None, Some(0), Some(9), None, None, Some(vec![5, 1, 3])).unwrap(), "0 0 9 * * 1,3,5");
    }

    #[test]
    fn test_weekly_invalid_dow() {
        let result = build("weekly", None, Some(0), Some(9), None, None, Some(vec![7]));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Invalid day_of_week"));
    }

    #[test]
    fn test_weekly_empty_dows() {
        let result = build("weekly", None, Some(0), Some(9), None, None, Some(vec![]));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("must not be empty"));
    }

    #[test]
    fn test_weekly_invalid_minute() {
        let result = build("weekly", None, Some(60), Some(9), None, None, Some(vec![1]));
        assert!(result.is_err());
    }

    #[test]
    fn test_weekly_invalid_hour() {
        let result = build("weekly", None, Some(0), Some(24), None, None, Some(vec![1]));
        assert!(result.is_err());
    }

    // --- Monthly tests ---
    #[test]
    fn test_monthly_default() {
        assert_eq!(build("monthly", None, None, None, None, None, None).unwrap(), "0 0 9 1 * *");
    }

    #[test]
    fn test_monthly_specific_time() {
        assert_eq!(build("monthly", None, Some(30), Some(14), Some(15), None, None).unwrap(), "0 30 14 15 * *");
    }

    #[test]
    fn test_monthly_with_second() {
        assert_eq!(build("monthly", Some(20), Some(30), Some(14), Some(15), None, None).unwrap(), "20 30 14 15 * *");
    }

    #[test]
    fn test_monthly_day_28() {
        assert_eq!(build("monthly", None, Some(0), Some(10), Some(28), None, None).unwrap(), "0 0 10 28 * *");
    }

    #[test]
    fn test_monthly_day_clamped_to_28() {
        assert_eq!(build("monthly", None, Some(0), Some(10), Some(31), None, None).unwrap(), "0 0 10 28 * *");
    }

    #[test]
    fn test_monthly_invalid_minute() {
        let result = build("monthly", None, Some(100), Some(10), Some(15), None, None);
        assert!(result.is_err());
    }

    #[test]
    fn test_monthly_invalid_hour() {
        let result = build("monthly", None, Some(0), Some(25), Some(15), None, None);
        assert!(result.is_err());
    }

    // --- Yearly tests ---
    #[test]
    fn test_yearly_default() {
        // Default: Jan 1st at 9:00
        assert_eq!(build("yearly", None, None, None, None, None, None).unwrap(), "0 0 9 1 1 *");
    }

    #[test]
    fn test_yearly_specific_date() {
        // June 15th at 14:30
        assert_eq!(build("yearly", None, Some(30), Some(14), Some(15), Some(6), None).unwrap(), "0 30 14 15 6 *");
    }

    #[test]
    fn test_yearly_with_second() {
        assert_eq!(build("yearly", Some(45), Some(0), Some(10), Some(1), Some(3), None).unwrap(), "45 0 10 1 3 *");
    }

    #[test]
    fn test_yearly_month_clamped() {
        // Month > 12 should be clamped to 12
        assert_eq!(build("yearly", None, Some(0), Some(9), Some(1), Some(15), None).unwrap(), "0 0 9 1 12 *");
    }

    #[test]
    fn test_yearly_day_clamped() {
        assert_eq!(build("yearly", None, Some(0), Some(9), Some(31), Some(6), None).unwrap(), "0 0 9 28 6 *");
    }

    #[test]
    fn test_yearly_invalid_minute() {
        let result = build("yearly", None, Some(60), Some(9), Some(1), Some(6), None);
        assert!(result.is_err());
    }

    #[test]
    fn test_yearly_invalid_hour() {
        let result = build("yearly", None, Some(0), Some(25), Some(1), Some(6), None);
        assert!(result.is_err());
    }

    // --- Invalid frequency tests ---
    #[test]
    fn test_invalid_frequency() {
        let result = build("biweekly", None, Some(0), Some(9), None, None, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Invalid frequency"));
    }

    #[test]
    fn test_empty_frequency() {
        let result = build("", None, None, None, None, None, None);
        assert!(result.is_err());
    }

    #[test]
    fn test_weekdays_not_supported() {
        let result = build("weekdays", None, Some(0), Some(9), None, None, None);
        assert!(result.is_err());
    }

    #[test]
    fn test_weekends_not_supported() {
        let result = build("weekends", None, Some(0), Some(9), None, None, None);
        assert!(result.is_err());
    }

    // --- Second validation tests ---
    #[test]
    fn test_invalid_second() {
        let result = build("daily", Some(60), None, Some(9), None, None, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Invalid second"));
    }

    #[test]
    fn test_second_boundary_59() {
        assert_eq!(build("daily", Some(59), Some(0), Some(9), None, None, None).unwrap(), "59 0 9 * * *");
    }
}
