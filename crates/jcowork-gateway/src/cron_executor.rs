//! Background cron executor — runs periodic tasks independently of WebSocket connections.
//!
//! Subscribes to the cron scheduler's reminder broadcast and, for each reminder
//! that has a `cron_job_id`, executes the LLM agent turn and stores the result.

use std::sync::Arc;
use std::sync::RwLock;

use jcowork_agent::r#loop as agent_loop;
use jcowork_agent::r#loop::{AgentOutputSink, AgentTurnOptions};
use jcowork_cron::{CronScheduler, TaskResult};
use jcowork_llm::LlmRouter;
use jcowork_logs::LogWriter;
use jcowork_memory::MemoryManager;
use jcowork_skills::SkillManager;
use jcowork_tools::base::ToolContext;
use jcowork_tools::registry::ToolRegistry;

/// A no-op output sink that discards streaming events.
/// Used by the background executor where there is no WebSocket client.
struct NoOpSink;

impl AgentOutputSink for NoOpSink {
    fn on_text_delta<'b>(&'b mut self, _text: &'b str) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'b>> {
        Box::pin(async {})
    }
    fn on_tool_call_start<'b>(&'b mut self, _name: &'b str, _call_id: &'b str, _arguments: &'b str) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'b>> {
        Box::pin(async {})
    }
    fn on_tool_call_end<'b>(&'b mut self, _name: &'b str, _result: &'b str) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'b>> {
        Box::pin(async {})
    }
    fn on_done<'b>(&'b mut self, _usage: Option<(i32, i32, i32)>) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'b>> {
        Box::pin(async {})
    }
    fn on_error<'b>(&'b mut self, _message: &'b str) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'b>> {
        Box::pin(async {})
    }
    fn on_status<'b>(&'b mut self, _message: &'b str) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'b>> {
        Box::pin(async {})
    }
}

/// Spawn the background cron executor task.
///
/// This task subscribes to reminder notifications and, for each cron-job
/// reminder, runs the LLM agent loop and stores the execution result.
/// It runs independently of any WebSocket connection, so periodic tasks
/// execute even when no client is connected.
pub fn spawn_cron_executor(
    cron_scheduler: Arc<CronScheduler>,
    llm_router: Arc<RwLock<LlmRouter>>,
    default_model: String,
    tool_registry: Arc<ToolRegistry>,
    memory_manager: Arc<MemoryManager>,
    skill_manager: Arc<SkillManager>,
    log_writer: Arc<LogWriter>,
    data_dir: String,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut reminder_rx = cron_scheduler.subscribe();
        tracing::info!("Background cron executor started");

        loop {
            match reminder_rx.recv().await {
                Ok(reminder) => {
                    // Only process reminders that are tied to a cron job
                    let cron_job_id = match &reminder.cron_job_id {
                        Some(id) => id.clone(),
                        None => continue,
                    };

                    let user_id = reminder.user_id.clone();
                    let prompt = reminder.prompt.clone().unwrap_or_default();
                    let model = reminder.model.clone().unwrap_or(default_model.clone());

                    tracing::info!(
                        cron_job_id = %cron_job_id,
                        user_id = %user_id,
                        model = %model,
                        prompt = %prompt,
                        "Cron executor: executing periodic task"
                    );

                    // Get LLM provider
                    let provider = {
                        let router = llm_router.read().unwrap();
                        router.get_provider(&model)
                    };
                    let provider = match provider {
                        Ok(p) => p,
                        Err(e) => {
                            tracing::error!(error = %e, model = %model, "Cron executor: failed to get LLM provider");
                            let result = TaskResult {
                                id: uuid::Uuid::new_v4().to_string(),
                                cron_job_id: cron_job_id.clone(),
                                user_id: user_id.clone(),
                                output: format!("Error: failed to get LLM provider: {}", e),
                                status: "error".to_string(),
                                executed_at: chrono::Utc::now().to_rfc3339(),
                            };
                            cron_scheduler.store_task_result(result).await;
                            continue;
                        }
                    };

                    // Build minimal conversation history
                    let mut history = vec![
                        jcowork_llm::provider::ChatMessage {
                            role: "system".to_string(),
                            content: "You are Jcowork Agent, an intelligent AI assistant.".to_string(),
                            tool_calls: None,
                            tool_call_id: None,
                            reasoning_content: None,
                        },
                        jcowork_llm::provider::ChatMessage {
                            role: "user".to_string(),
                            content: prompt.clone(),
                            tool_calls: None,
                            tool_call_id: None,
                            reasoning_content: None,
                        },
                    ];

                    // Build tools (filtered by skills)
                    let (_skill_prompt, enabled_skill_ids) =
                        agent_loop::build_skill_prompt(&memory_manager, &skill_manager, &user_id).await;
                    let tools = agent_loop::filter_tools_by_skill(
                        &tool_registry.all_schemas(),
                        &enabled_skill_ids,
                    );

                    let workspace_root = format!("{}/{}/workspace", data_dir, user_id);
                    let _ = tokio::fs::create_dir_all(&workspace_root).await;
                    let tool_ctx = ToolContext {
                        user_id: user_id.clone(),
                        workspace_root,
                    };

                    let mut sink = NoOpSink;

                    let result = agent_loop::run_turn(AgentTurnOptions {
                        history: &mut history,
                        tools: &tools,
                        provider,
                        tool_registry: tool_registry.clone(),
                        tool_ctx: &tool_ctx,
                        pre_context: None,
                        max_turns: 5,
                        llm_timeout_secs: 120,
                        stream_timeout_secs: 120,
                        tool_timeout_secs: 30,
                        output: &mut sink,
                        user_id: &user_id,
                        model: &model,
                        log_writer: Some(log_writer.clone()),
                    })
                    .await;

                    // Store execution result
                    let (output, status) = if result.completed {
                        (result.response, "success".to_string())
                    } else {
                        (result.response, "error".to_string())
                    };

                    tracing::info!(
                        cron_job_id = %cron_job_id,
                        status = %status,
                        output_len = output.len(),
                        "Cron executor: task completed"
                    );

                    let task_result = TaskResult {
                        id: uuid::Uuid::new_v4().to_string(),
                        cron_job_id: cron_job_id.clone(),
                        user_id: user_id.clone(),
                        output,
                        status,
                        executed_at: chrono::Utc::now().to_rfc3339(),
                    };
                    cron_scheduler.store_task_result(task_result).await;
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(skipped = n, "Cron executor: missed {} broadcast messages", n);
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    tracing::info!("Cron executor: broadcast channel closed, shutting down");
                    break;
                }
            }
        }
    })
}
