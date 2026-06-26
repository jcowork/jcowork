//! Cron and Reminder tools - schedule tasks and one-time reminders.

use anyhow::Result;
use async_trait::async_trait;
use jcowork_cron::CronScheduler;
use std::sync::Arc;

use crate::base::{Tool, ToolContext};

/// Add a one-time reminder.
pub struct ReminderAddTool {
    pub scheduler: Arc<CronScheduler>,
}

#[async_trait]
impl Tool for ReminderAddTool {
    fn name(&self) -> &str { "reminder_add" }
    fn description(&self) -> &str {
        "Set a one-time reminder that will notify the user at the specified time. \
         Use this when the user asks to be reminded of something at a specific time. \
         The fire_at must be an ISO 8601 datetime string. \
         If the user says '11:41', calculate the full ISO datetime using today's date and the user's timezone (default: Asia/Shanghai, UTC+8). \
         If the time has already passed today, use tomorrow's date. \
         When the user wants to be reminded to do something (e.g., 'remind me to search for news'), \
         set the action parameter to the task they want to perform, so it can be executed automatically when the reminder fires."
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "fire_at": {
                    "type": "string",
                    "description": "ISO 8601 datetime for when to trigger the reminder (e.g., '2026-05-15T11:41:00+08:00'). Always include timezone offset."
                },
                "message": {
                    "type": "string",
                    "description": "The reminder message to show the user at the specified time"
                },
                "action": {
                    "type": "string",
                    "description": "Optional: The action to execute when the reminder fires (e.g., 'search 俄乌战争最新进展'). If set, this action will be automatically triggered."
                }
            },
            "required": ["fire_at", "message"]
        })
    }
    async fn execute(&self, args: &str, ctx: &ToolContext) -> Result<String> {
        let params: serde_json::Value = serde_json::from_str(args)?;
        let fire_at = params["fire_at"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'fire_at' parameter"))?;
        let message = params["message"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'message' parameter"))?;
        let action = params["action"].as_str();

        match self.scheduler.add_reminder(&ctx.user_id, fire_at, message, action).await {
            Ok(id) => Ok(format!("Reminder set! ID: {}\nWill remind at: {}\nMessage: {}", id, fire_at, message)),
            Err(e) => Ok(format!("Failed to set reminder: {}", e)),
        }
    }
}

/// List user's reminders.
pub struct ReminderListTool {
    pub scheduler: Arc<CronScheduler>,
}

#[async_trait]
impl Tool for ReminderListTool {
    fn name(&self) -> &str { "reminder_list" }
    fn description(&self) -> &str { "List all active (untriggered) reminders for the current user." }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object", "properties": {} })
    }
    async fn execute(&self, _args: &str, ctx: &ToolContext) -> Result<String> {
        let reminders = self.scheduler.list_reminders(&ctx.user_id).await;
        if reminders.is_empty() {
            return Ok("No active reminders.".to_string());
        }
        let mut lines = vec!["Active reminders:".to_string()];
        for r in &reminders {
            lines.push(format!("- ID: {} | At: {} | Message: {}", r.id, r.fire_at, r.message));
        }
        Ok(lines.join("\n"))
    }
}

/// Remove a reminder.
pub struct ReminderRemoveTool {
    pub scheduler: Arc<CronScheduler>,
}

#[async_trait]
impl Tool for ReminderRemoveTool {
    fn name(&self) -> &str { "reminder_remove" }
    fn description(&self) -> &str { "Remove a previously set reminder by its ID." }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "The reminder ID to remove" }
            },
            "required": ["id"]
        })
    }
    async fn execute(&self, args: &str, _ctx: &ToolContext) -> Result<String> {
        let params: serde_json::Value = serde_json::from_str(args)?;
        let id = params["id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'id' parameter"))?;
        match self.scheduler.remove_reminder(id).await {
            Ok(()) => Ok(format!("Reminder {} removed.", id)),
            Err(e) => Ok(format!("Failed to remove reminder: {}", e)),
        }
    }
}

/// Add a cron job (recurring schedule).
pub struct CronAddTool {
    pub scheduler: Arc<CronScheduler>,
}

#[async_trait]
impl Tool for CronAddTool {
    fn name(&self) -> &str { "cron_add" }
    fn description(&self) -> &str {
        "Schedule a recurring task using cron syntax. The agent will send a reminder notification on schedule. \
         Cron expression format: minute hour day-of-month month day-of-week. \
         Examples: '0 9 * * *' = daily at 9am, '0 6,18 * * *' = twice daily at 6am and 6pm, \
         '0 9 * * 1-5' = weekdays at 9am. Use 5-field standard cron syntax (no seconds)."
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "schedule": { "type": "string", "description": "Cron schedule expression (e.g., '0 9 * * *' for daily at 9am)" },
                "prompt": { "type": "string", "description": "The reminder message to send on schedule" }
            },
            "required": ["schedule", "prompt"]
        })
    }
    async fn execute(&self, args: &str, ctx: &ToolContext) -> Result<String> {
        let params: serde_json::Value = serde_json::from_str(args)?;
        let schedule = params["schedule"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'schedule' parameter"))?;
        let prompt = params["prompt"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'prompt' parameter"))?;

        match self.scheduler.add_cron_job(&ctx.user_id, schedule, prompt).await {
            Ok(id) => {
                let next = CronScheduler::next_fire_time(schedule)
                    .map(|t| t.to_rfc3339())
                    .unwrap_or_else(|e| format!("unknown: {}", e));
                Ok(format!("Cron job added! ID: {}\nSchedule: {}\nNext run: {}\nMessage: {}", id, schedule, next, prompt))
            }
            Err(e) => Ok(format!("Failed to add cron job: {}", e)),
        }
    }
}

/// List cron jobs.
pub struct CronListTool {
    pub scheduler: Arc<CronScheduler>,
}

#[async_trait]
impl Tool for CronListTool {
    fn name(&self) -> &str { "cron_list" }
    fn description(&self) -> &str { "List all scheduled cron jobs." }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object", "properties": {} })
    }
    async fn execute(&self, _args: &str, ctx: &ToolContext) -> Result<String> {
        let jobs = self.scheduler.list_cron_jobs(&ctx.user_id).await;
        if jobs.is_empty() {
            return Ok("No cron jobs.".to_string());
        }
        let mut lines = vec!["Active cron jobs:".to_string()];
        for j in &jobs {
            lines.push(format!("- ID: {} | Schedule: {} | Message: {}", j.id, j.schedule, j.prompt));
        }
        Ok(lines.join("\n"))
    }
}

/// Remove a cron job.
pub struct CronRemoveTool {
    pub scheduler: Arc<CronScheduler>,
}

#[async_trait]
impl Tool for CronRemoveTool {
    fn name(&self) -> &str { "cron_remove" }
    fn description(&self) -> &str { "Remove a scheduled cron job by ID." }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "Cron job ID to remove" }
            },
            "required": ["id"]
        })
    }
    async fn execute(&self, args: &str, _ctx: &ToolContext) -> Result<String> {
        let params: serde_json::Value = serde_json::from_str(args)?;
        let id = params["id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'id' parameter"))?;
        match self.scheduler.remove_cron_job(id).await {
            Ok(()) => Ok(format!("Cron job {} removed.", id)),
            Err(e) => Ok(format!("Failed to remove cron job: {}", e)),
        }
    }
}
