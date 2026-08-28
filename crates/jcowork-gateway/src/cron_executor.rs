//! Background cron executor — runs periodic tasks independently of WebSocket connections.
//!
//! Subscribes to the cron scheduler's reminder broadcast and, for each reminder
//! that has a `cron_job_id`, executes the LLM agent turn and stores the result.

use std::sync::Arc;
use std::sync::RwLock;

use futures::FutureExt;
use jcowork_agent::r#loop as agent_loop;
use jcowork_agent::r#loop::{AgentOutputSink, AgentTurnOptions};
use jcowork_cron::{CronScheduler, TaskResult};
use jcowork_llm::LlmRouter;
use jcowork_logs::LogWriter;
use jcowork_memory::MemoryManager;
use jcowork_skills::SkillManager;
use jcowork_tools::base::ToolContext;
use jcowork_tools::registry::ToolRegistry;

/// A logging output sink that captures the final response and errors.
/// Used by the background executor where there is no WebSocket client.
struct LogSink {
    response: String,
    error: Option<String>,
}

impl LogSink {
    fn new() -> Self {
        Self {
            response: String::new(),
            error: None,
        }
    }
}

impl AgentOutputSink for LogSink {
    fn on_text_delta<'b>(&'b mut self, text: &'b str) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'b>> {
        self.response.push_str(text);
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
    fn on_error<'b>(&'b mut self, message: &'b str) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'b>> {
        self.error = Some(message.to_string());
        Box::pin(async {})
    }
    fn on_status<'b>(&'b mut self, _message: &'b str) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'b>> {
        Box::pin(async {})
    }
}

/// Store a task result, always ensuring a record is created.
async fn store_result(
    cron_scheduler: &CronScheduler,
    cron_job_id: &str,
    user_id: &str,
    output: String,
    status: &str,
) {
    let task_result = TaskResult {
        id: uuid::Uuid::new_v4().to_string(),
        cron_job_id: cron_job_id.to_string(),
        user_id: user_id.to_string(),
        output,
        status: status.to_string(),
        executed_at: chrono::Utc::now().to_rfc3339(),
    };
    cron_scheduler.store_task_result(task_result).await;
    tracing::info!(
        cron_job_id = %cron_job_id,
        status = %status,
        "Cron executor: result stored"
    );
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
        tracing::info!("Background cron executor started and subscribed to reminders");

        loop {
            let reminder = match reminder_rx.recv().await {
                Ok(r) => r,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(skipped = n, "Cron executor: missed broadcast messages");
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    tracing::warn!("Cron executor: broadcast channel closed");
                    break;
                }
            };

            // Only process reminders tied to a cron job
            let cron_job_id = match &reminder.cron_job_id {
                Some(id) => id.clone(),
                None => continue,
            };

            let user_id = reminder.user_id.clone();
            let prompt = reminder.prompt.clone().unwrap_or_else(|| reminder.message.clone());
            let model = reminder.model.clone().unwrap_or(default_model.clone());

            tracing::info!(
                cron_job_id = %cron_job_id,
                user_id = %user_id,
                model = %model,
                prompt_len = prompt.len(),
                "Cron executor: received trigger, starting execution"
            );

            // Execute with panic protection to ensure a result is always stored
            let exec_result = std::panic::AssertUnwindSafe(execute_cron_task(
                &cron_scheduler,
                &llm_router,
                &tool_registry,
                &memory_manager,
                &skill_manager,
                &log_writer,
                &data_dir,
                &cron_job_id,
                &user_id,
                &prompt,
                &model,
            ))
            .catch_unwind()
            .await;

            match exec_result {
                Ok(Ok(())) => {
                    // execute_cron_task already stored the result
                }
                Ok(Err(e)) => {
                    tracing::error!(error = %e, cron_job_id = %cron_job_id, "Cron executor: execution failed");
                    store_result(
                        &cron_scheduler,
                        &cron_job_id,
                        &user_id,
                        format!("Execution failed: {}", e),
                        "error",
                    )
                    .await;
                }
                Err(panic_info) => {
                    let msg = if let Some(s) = panic_info.downcast_ref::<&str>() {
                        s.to_string()
                    } else if let Some(s) = panic_info.downcast_ref::<String>() {
                        s.clone()
                    } else {
                        "Unknown panic".to_string()
                    };
                    tracing::error!(panic = %msg, cron_job_id = %cron_job_id, "Cron executor: PANIC during execution");
                    store_result(
                        &cron_scheduler,
                        &cron_job_id,
                        &user_id,
                        format!("Internal error: {}", msg),
                        "error",
                    )
                    .await;
                }
            }
        }
    })
}

/// Execute a single cron task: call LLM and store the result.
/// Returns Ok(()) if the result was stored successfully, Err otherwise.
async fn execute_cron_task(
    cron_scheduler: &CronScheduler,
    llm_router: &RwLock<LlmRouter>,
    tool_registry: &Arc<ToolRegistry>,
    memory_manager: &MemoryManager,
    skill_manager: &SkillManager,
    log_writer: &Arc<LogWriter>,
    data_dir: &str,
    cron_job_id: &str,
    user_id: &str,
    prompt: &str,
    model: &str,
) -> Result<(), String> {
    // Resolve LLM provider
    let provider = {
        let router = llm_router.read().map_err(|e| format!("Router lock poisoned: {}", e))?;
        router.get_provider(model).map_err(|e| format!("Unknown provider for model '{}': {}. Check that the provider is configured with a valid API key.", model, e))
    }?;

    // Build conversation history
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
            content: prompt.to_string(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        },
    ];

    // Build tools filtered by user's enabled skills
    let (_skill_prompt, enabled_skill_ids) =
        agent_loop::build_skill_prompt(memory_manager, skill_manager, user_id).await;
    let tools = agent_loop::filter_tools_by_skill(
        &tool_registry.all_schemas(),
        &enabled_skill_ids,
    );

    // Ensure workspace directory exists
    let workspace_root = format!("{}/{}/workspace", data_dir, user_id);
    let _ = tokio::fs::create_dir_all(&workspace_root).await;
    let tool_ctx = ToolContext {
        user_id: user_id.to_string(),
        workspace_root,
    };

    let mut sink = LogSink::new();

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
        tool_timeout_secs: 60,
        output: &mut sink,
        user_id,
        model,
        log_writer: Some(log_writer.clone()),
    })
    .await;

    // Determine output and status
    let (output, status) = if result.completed && !result.response.is_empty() {
        (result.response, "success")
    } else if let Some(err) = sink.error {
        (format!("{}\n\n(LLM did not produce a response)", err), "error")
    } else if result.response.is_empty() {
        ("The LLM returned an empty response. It may have failed to generate content.".to_string(), "error")
    } else {
        (result.response, "error")
    };

    tracing::info!(
        cron_job_id = %cron_job_id,
        status = %status,
        output_len = output.len(),
        turns = result.turns_used,
        "Cron executor: task finished"
    );

    // Always store a result
    let task_result = TaskResult {
        id: uuid::Uuid::new_v4().to_string(),
        cron_job_id: cron_job_id.to_string(),
        user_id: user_id.to_string(),
        output,
        status: status.to_string(),
        executed_at: chrono::Utc::now().to_rfc3339(),
    };
    cron_scheduler.store_task_result(task_result).await;

    Ok(())
}
