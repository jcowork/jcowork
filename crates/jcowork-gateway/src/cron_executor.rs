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
use jcowork_storage::user_store::TRASH_RETENTION_DAYS;
use jcowork_storage::UserStore;
use jcowork_tools::base::ToolContext;
use jcowork_tools::registry::ToolRegistry;

/// Maximum characters kept for a single tool argument excerpt in the trace.
const TRACE_ARG_MAX: usize = 240;
/// Maximum characters kept for a single tool result excerpt in the trace.
const TRACE_RESULT_MAX: usize = 320;
/// Maximum characters kept for an assistant text excerpt in the trace.
const TRACE_TEXT_MAX: usize = 500;
/// Overall cap for the rendered trace appended to a failed result.
const TRACE_TOTAL_MAX: usize = 60_000;

/// Flatten whitespace and truncate to `max` chars on a char boundary.
fn excerpt(s: &str, max: usize) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > max {
        let cut: String = flat.chars().take(max).collect();
        format!("{}…", cut)
    } else {
        flat
    }
}

/// A tool call that has started but not finished yet.
struct PendingTool {
    name: String,
    args: String,
    started: std::time::Instant,
}

/// A logging output sink that captures the final response and errors.
/// Used by the background executor where there is no WebSocket client.
///
/// It also records a markdown turn-by-turn trace (assistant text snippets,
/// tool calls with arguments/results, errors). The trace is appended to the
/// stored output when a run fails, so the reason can be judged from the
/// execution record alone.
struct LogSink {
    response: String,
    error: Option<String>,
    /// Rendered trace of all completed turns.
    trace: String,
    /// Current turn number (0 = no turn started yet).
    turn: usize,
    /// Assistant text streamed during the current turn.
    turn_text: String,
    /// Tool/error lines of the current turn, in event order.
    turn_lines: Vec<String>,
    /// Tool calls that started but have not finished yet.
    pending_tools: Vec<PendingTool>,
}

impl LogSink {
    fn new() -> Self {
        Self {
            response: String::new(),
            error: None,
            trace: String::new(),
            turn: 0,
            turn_text: String::new(),
            turn_lines: Vec::new(),
            pending_tools: Vec::new(),
        }
    }

    /// Render the current turn (if any) into the trace buffer.
    fn flush_turn(&mut self) {
        if self.turn > 0 {
            let mut body = String::new();
            if !self.turn_text.trim().is_empty() {
                body.push_str(&format!("text: {}\n", excerpt(&self.turn_text, TRACE_TEXT_MAX)));
            }
            for line in self.turn_lines.drain(..) {
                body.push_str(&line);
                body.push('\n');
            }
            for pending in self.pending_tools.drain(..) {
                body.push_str(&format!(
                    "- **{}** args: {} (no result recorded)\n",
                    pending.name,
                    excerpt(&pending.args, TRACE_ARG_MAX)
                ));
            }
            if !body.is_empty() {
                self.trace.push_str(&format!("\n**Turn {}**\n{}", self.turn, body));
            }
        }
        self.turn_text.clear();
        self.turn_lines.clear();
        self.pending_tools.clear();
    }

    /// Build the markdown trace of the whole run. Empty when nothing was
    /// recorded.
    fn render_trace(&mut self) -> String {
        self.flush_turn();
        if self.trace.trim().is_empty() {
            return String::new();
        }
        let mut out = String::from("\n\n---\n\n**Turn-by-turn trace**\n");
        out.push_str(&self.trace);
        if out.chars().count() > TRACE_TOTAL_MAX {
            let cut: String = out.chars().take(TRACE_TOTAL_MAX).collect();
            out = format!("{}\n\n(trace truncated)", cut);
        }
        out
    }
}

impl AgentOutputSink for LogSink {
    fn on_text_delta<'b>(&'b mut self, text: &'b str) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'b>> {
        self.response.push_str(text);
        self.turn_text.push_str(text);
        Box::pin(async {})
    }
    fn on_tool_call_start<'b>(&'b mut self, name: &'b str, _call_id: &'b str, arguments: &'b str) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'b>> {
        self.pending_tools.push(PendingTool {
            name: name.to_string(),
            args: arguments.to_string(),
            started: std::time::Instant::now(),
        });
        Box::pin(async {})
    }
    fn on_tool_call_end<'b>(&'b mut self, name: &'b str, result: &'b str) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'b>> {
        let pending = match self.pending_tools.iter().position(|p| p.name == name) {
            Some(pos) => Some(self.pending_tools.remove(pos)),
            None if !self.pending_tools.is_empty() => Some(self.pending_tools.remove(0)),
            None => None,
        };
        let (args, secs) = match pending {
            Some(p) => (p.args, p.started.elapsed().as_secs_f64()),
            None => (String::new(), 0.0),
        };
        let args_shown = if args.is_empty() {
            "(unknown)".to_string()
        } else {
            excerpt(&args, TRACE_ARG_MAX)
        };
        self.turn_lines.push(format!(
            "- **{}** args: {} → {:.1}s, {} chars: {}",
            name,
            args_shown,
            secs,
            result.chars().count(),
            excerpt(result, TRACE_RESULT_MAX)
        ));
        Box::pin(async {})
    }
    fn on_done<'b>(&'b mut self, _usage: Option<(i32, i32, i32)>) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'b>> {
        Box::pin(async {})
    }
    fn on_error<'b>(&'b mut self, message: &'b str) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'b>> {
        self.error = Some(message.to_string());
        self.turn_lines.push(format!("- ERROR: {}", excerpt(message, TRACE_RESULT_MAX)));
        Box::pin(async {})
    }
    fn on_status<'b>(&'b mut self, message: &'b str) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'b>> {
        // The agent loop emits one status per turn ("🤖 ..." / "🔄 ... 第N轮")
        // plus a "🔧 正在执行工具..." notice inside a turn. Everything except
        // the tool notice marks the start of a new turn.
        if !message.starts_with('🔧') {
            self.flush_turn();
            self.turn += 1;
        }
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
    user_store: Arc<UserStore>,
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

            // Skip reminders whose owning account no longer exists or is in
            // the recycle bin — trashed users must not run tasks.
            match user_store.get_user_by_id(&user_id).await {
                Ok(Some(user)) if user.deleted_at.is_none() => {}
                Ok(Some(_)) => {
                    tracing::warn!(
                        cron_job_id = %cron_job_id,
                        user_id = %user_id,
                        "Cron executor: owner is in the trash, skipping execution"
                    );
                    continue;
                }
                Ok(None) => {
                    tracing::warn!(
                        cron_job_id = %cron_job_id,
                        user_id = %user_id,
                        "Cron executor: owner account not found, skipping execution"
                    );
                    continue;
                }
                Err(e) => {
                    tracing::warn!(
                        cron_job_id = %cron_job_id,
                        user_id = %user_id,
                        error = %e,
                        "Cron executor: failed to verify owner account, skipping execution"
                    );
                    continue;
                }
            }

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
            images: None,
        },
        jcowork_llm::provider::ChatMessage {
            role: "user".to_string(),
            content: prompt.to_string(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            images: None,
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
        mentioned_public_users: Vec::new(),
        model: Some(model.to_string()),
    };

    let mut sink = LogSink::new();

    let result = agent_loop::run_turn(AgentTurnOptions {
        history: &mut history,
        tools: &tools,
        provider,
        tool_registry: tool_registry.clone(),
        tool_ctx: &tool_ctx,
        pre_context: None,
        max_turns: 60,
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
    let sink_error = sink.error.take();
    let (mut output, status) = if result.completed && !result.response.is_empty() {
        (result.response, "success")
    } else if let Some(err) = sink_error {
        (format!("{}\n\n(LLM did not produce a response)", err), "error")
    } else if !result.completed {
        // The loop used all its turns while still calling tools — the model
        // was working on the task but never produced a final text answer.
        (
            format!(
                "Task stopped after reaching the maximum of {} turns without producing a final response. \
                 Consider simplifying the instruction or splitting it into smaller tasks.",
                result.turns_used
            ),
            "error",
        )
    } else if result.response.is_empty() {
        ("The LLM returned an empty response. It may have failed to generate content.".to_string(), "error")
    } else {
        (result.response, "error")
    };

    // On failure, append the per-turn trace so the stored record alone is
    // enough to judge where the run went wrong.
    if status != "success" {
        output.push_str(&sink.render_trace());
    }

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

/// Spawn the background trash purger.
///
/// Runs once immediately at startup, then every `interval` (recommended:
/// hourly): permanently deletes account records that have been in the
/// recycle bin for more than `TRASH_RETENTION_DAYS` days. Only the `users`
/// table rows are removed — per-user data directories stay on disk.
pub fn spawn_trash_purger(
    user_store: Arc<UserStore>,
    interval: std::time::Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        tracing::info!(
            retention_days = TRASH_RETENTION_DAYS,
            "Trash purger started"
        );

        loop {
            match user_store.purge_expired_trash(TRASH_RETENTION_DAYS).await {
                Ok(count) if count > 0 => {
                    tracing::info!(
                        purged = count,
                        "Trash purger: permanently removed expired trashed accounts"
                    );
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::error!(error = %e, "Trash purger: purge failed");
                }
            }
            tokio::time::sleep(interval).await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;

    #[test]
    fn trace_records_turns_tools_and_errors() {
        let mut sink = LogSink::new();
        // Turn 1: status → text → tool start → tool notice → tool end.
        block_on(sink.on_status("🤖 正在调用 deepseek ..."));
        block_on(sink.on_text_delta("I will check the file."));
        block_on(sink.on_tool_call_start("file_read", "c1", "{\"path\": \"a.html\"}"));
        block_on(sink.on_status("🔧 正在执行工具: file_read"));
        block_on(sink.on_tool_call_end("file_read", "1234 chars of content"));
        // Turn 2: continues, then errors.
        block_on(sink.on_status("🔄 工具调用完成，继续思考 (第2轮)..."));
        block_on(sink.on_tool_call_start("web_search", "c2", "{\"query\": \"latest ai models\"}"));
        block_on(sink.on_tool_call_end("web_search", "search results..."));
        block_on(sink.on_error("LLM error: something bad"));

        let trace = sink.render_trace();
        assert!(trace.contains("**Turn 1**"), "trace: {}", trace);
        assert!(trace.contains("**Turn 2**"), "trace: {}", trace);
        assert!(trace.contains("file_read"));
        assert!(trace.contains("web_search"));
        assert!(trace.contains("ERROR: LLM error: something bad"));
        assert!(trace.contains("I will check the file."));
        assert_eq!(sink.response, "I will check the file.");
    }

    #[test]
    fn trace_truncates_long_excerpts() {
        let mut sink = LogSink::new();
        block_on(sink.on_status("🤖 ..."));
        block_on(sink.on_tool_call_start("shell", "c1", "{\"command\": \"echo\"}"));
        block_on(sink.on_tool_call_end("shell", &"x".repeat(1000)));
        let trace = sink.render_trace();
        assert!(trace.contains("1000 chars"), "trace: {}", trace);
        assert!(trace.contains('…'));
    }

    #[test]
    fn trace_is_empty_without_events() {
        let mut sink = LogSink::new();
        assert_eq!(sink.render_trace(), "");
    }
}
