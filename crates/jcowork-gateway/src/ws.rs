//! WebSocket handler for real-time agent communication.

use axum::extract::ws::{Message, WebSocket};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

use jcowork_cron::{CronScheduler, CronJob, Reminder};
use jcowork_llm::provider::{ChatMessage, StreamChunk, ToolCall};
use jcowork_llm::LlmRouter;
use jcowork_logs::{build_llm_input, build_llm_output, LogEntry, LogWriter, ToolCallEntry};
use jcowork_memory::MemoryManager;
use jcowork_skills::{builtin_skills, SkillManager};
use jcowork_tools::base::ToolContext;
use jcowork_tools::cron::{ReminderAddTool, ReminderListTool, ReminderRemoveTool, CronAddTool, CronListTool, CronRemoveTool};
use jcowork_tools::bing_search::WebSearchTool;
use jcowork_tools::memory::{MemorySaveTool, MemoryUpdateTool, MemoryRecallTool, MemorySearchTool};
use jcowork_tools::pdf_parse::PdfParseTool;
use jcowork_tools::report_search::{ReportListCompaniesTool, ReportSearchTool};
use jcowork_tools::registry::ToolRegistry;

use crate::session::SessionManager;

/// Incoming WebSocket message from client.
#[derive(Debug, Deserialize)]
pub struct WsInput {
    pub session_id: Option<String>,
    pub content: String,
    /// Model string in "provider:model" format. Defaults to server default if not provided.
    pub model: Option<String>,
}

/// Outgoing WebSocket message to client.
#[derive(Debug, Serialize)]
#[serde(tag = "type")]
pub enum WsOutput {
    #[serde(rename = "text_delta")]
    TextDelta { content: String },
    #[serde(rename = "tool_call_start")]
    ToolCallStart { name: String, call_id: String },
    #[serde(rename = "tool_call_end")]
    ToolCallEnd { name: String, result: String },
    #[serde(rename = "reminder")]
    Reminder { id: String, message: String, fire_at: String },
    #[serde(rename = "done")]
    Done { usage: UsageInfo },
    #[serde(rename = "error")]
    Error { message: String },
}

#[derive(Debug, Serialize)]
pub struct UsageInfo {
    pub prompt_tokens: i32,
    pub completion_tokens: i32,
    pub total_tokens: i32,
}

/// Build the tool registry with the given CronScheduler and MemoryManager.
pub fn build_tool_registry(scheduler: Arc<CronScheduler>, memory_manager: Arc<MemoryManager>) -> Arc<ToolRegistry> {
    let mut registry = ToolRegistry::new();
    // Reminder/Cron tools
    registry.register(Arc::new(ReminderAddTool { scheduler: scheduler.clone() }));
    registry.register(Arc::new(ReminderListTool { scheduler: scheduler.clone() }));
    registry.register(Arc::new(ReminderRemoveTool { scheduler: scheduler.clone() }));
    registry.register(Arc::new(CronAddTool { scheduler: scheduler.clone() }));
    registry.register(Arc::new(CronListTool { scheduler: scheduler.clone() }));
    registry.register(Arc::new(CronRemoveTool { scheduler }));
    // Memory tools
    registry.register(Arc::new(MemorySaveTool { manager: memory_manager.clone() }));
    registry.register(Arc::new(MemoryUpdateTool { manager: memory_manager.clone() }));
    registry.register(Arc::new(MemoryRecallTool { manager: memory_manager.clone() }));
    registry.register(Arc::new(MemorySearchTool { manager: memory_manager }));
    // PDF parsing tool
    registry.register(Arc::new(PdfParseTool::default()));
    // Web search tool
    registry.register(Arc::new(WebSearchTool::default()));
    // Report search tools
    registry.register(Arc::new(ReportSearchTool::default()));
    registry.register(Arc::new(ReportListCompaniesTool::default()));
    Arc::new(registry)
}

/// Handle a WebSocket connection for a specific user.
pub async fn ws_handler(
    ws: WebSocket,
    user_id: String,
    session_manager: Arc<SessionManager>,
    llm_router: Arc<LlmRouter>,
    default_model: String,
    tool_registry: Arc<ToolRegistry>,
    cron_scheduler: Arc<CronScheduler>,
    log_writer: Arc<LogWriter>,
    memory_manager: Arc<MemoryManager>,
    skill_manager: Arc<SkillManager>,
) {
    let (mut ws_sender, mut ws_receiver) = ws.split();

    // Get or create UserActor for this user
    let _actor = session_manager.get(&user_id);

    // Load user's custom agent identity from memory
    let custom_identity = load_agent_identity(&memory_manager, &user_id).await;

    // Load enabled skills and build skill prompt blocks
    let (skill_prompt, enabled_skill_ids) = build_skill_prompt(&memory_manager, &skill_manager, &user_id).await;

    // Conversation history for this connection
    let mut history: Vec<ChatMessage> = Vec::new();

    // System prompt (uses custom identity if set, otherwise default)
    let system_prompt = build_system_prompt_with_identity(custom_identity.as_deref(), &skill_prompt);
    history.push(ChatMessage {
        role: "system".to_string(),
        content: system_prompt,
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
    });

    // Subscribe to reminder notifications for this user
    let mut reminder_rx = cron_scheduler.subscribe();
    let user_id_for_reminder = user_id.clone();

    loop {
        tokio::select! {
            // Incoming WebSocket messages from client
            msg = ws_receiver.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                // Parse incoming message
                let input: WsInput = match serde_json::from_str(&text) {
                    Ok(v) => v,
                    Err(_) => {
                        let _ = ws_sender
                            .send(Message::Text(
                                serde_json::json!({"type": "error", "message": "Invalid JSON"})
                                    .to_string()
                                    .into(),
                            ))
                            .await;
                        continue;
                    }
                };

                // Add user message to history
                history.push(ChatMessage {
                    role: "user".to_string(),
                    content: input.content.clone(),
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                });

                // Resolve model string
                let model_str = input.model.as_deref().unwrap_or(&default_model);

                // Get provider
                let provider = match llm_router.get_provider(model_str) {
                    Ok(p) => p,
                    Err(e) => {
                        let _ = ws_sender
                            .send(Message::Text(
                                serde_json::json!({"type": "error", "message": e.to_string()})
                                    .to_string()
                                    .into(),
                            ))
                            .await;
                        history.pop();
                        continue;
                    }
                };

                // Get tool schemas — filter out skill-gated tools if the skill is not enabled
                let tools: Vec<_> = tool_registry.all_schemas()
                    .into_iter()
                    .filter(|t| {
                        // Skill-gated tools: only available when the corresponding skill is enabled
                        let skill_required = match t.function.name.as_str() {
                            "web_search" => Some("builtin:web_search"),
                            "report_search" | "report_list_companies" => Some("builtin:write_research_report"),
                            _ => None,
                        };
                        match skill_required {
                            Some(skill_id) => enabled_skill_ids.contains(skill_id),
                            None => true,
                        }
                    })
                    .collect();
                let tool_ctx = ToolContext {
                    user_id: user_id.clone(),
                    workspace_root: String::new(),
                };

                // Fetch active reminders and cron jobs to inject as context
                let active_reminders = cron_scheduler.list_reminders(&user_id).await;
                let active_cron_jobs = cron_scheduler.list_cron_jobs(&user_id).await;
                let reminder_ctx_msg = build_reminder_context_msg(&active_reminders, &active_cron_jobs);


                // Agent loop: keep calling LLM until it stops making tool calls
                let max_turns = 10;
                for _turn in 0..max_turns {
                    // Build effective history: inject reminder/cron context right after system prompt
                    let effective_history = match &reminder_ctx_msg {
                        Some(ctx) if history.len() >= 1 => {
                            let mut h = history.clone();
                            h.insert(1, ctx.clone());
                            h
                        }
                        _ => history.clone(),

                    };

                    // Call LLM with streaming
                    let llm_start = std::time::Instant::now();
                    let llm_input = build_llm_input(&effective_history.iter().map(|m| (m.role.as_str(), m.content.as_str())).collect::<Vec<_>>());
                    let provider_name = provider.name().to_string();
                    let stream_result = provider.chat_stream(&effective_history, &tools).await;
                    let mut stream = match stream_result {
                        Ok(s) => s,
                        Err(e) => {
                            let _ = ws_sender
                                .send(Message::Text(
                                    serde_json::json!({"type": "error", "message": format!("LLM error: {}", e)})
                                        .to_string()
                                        .into(),
                                ))
                                .await;
                            break;
                        }
                    };

                    let mut assistant_content = String::new();
                    let mut reasoning_content = String::new();
                    let mut current_tool_args: HashMap<String, (String, String, String)> = HashMap::new();
                    let mut tool_call_started: std::collections::HashSet<String> = std::collections::HashSet::new();
                    let mut had_error = false;
                    let mut final_usage: Option<(i32, i32, i32)> = None;

                    while let Some(chunk) = stream.next().await {
                        match chunk {
                            Ok(StreamChunk::Delta(delta)) => {
                                assistant_content.push_str(&delta);
                                let _ = ws_sender
                                    .send(Message::Text(
                                        serde_json::json!({"type": "text_delta", "content": delta})
                                            .to_string()
                                            .into(),
                                    ))
                                    .await;
                            }
                            Ok(StreamChunk::ReasoningDelta(reasoning)) => {
                                reasoning_content.push_str(&reasoning);
                                // Don't send reasoning to client, just accumulate for history
                            }
                            Ok(StreamChunk::ToolCallDelta(call_id, name, args_delta)) => {
                                let entry = current_tool_args
                                    .entry(call_id.clone())
                                    .or_insert_with(|| (call_id.clone(), name.clone(), String::new()));
                                entry.2.push_str(&args_delta);
                                // Only send tool_call_start once per call_id
                                if tool_call_started.insert(call_id.clone()) {
                                    let _ = ws_sender
                                        .send(Message::Text(
                                            serde_json::json!({
                                                "type": "tool_call_start",
                                                "name": name,
                                                "call_id": call_id
                                            })
                                                .to_string()
                                                .into(),
                                        ))
                                        .await;
                                }
                            }
                            Ok(StreamChunk::Done(usage)) => {
                                // Accumulate usage; only forward `done` to client after the final
                                // turn (i.e., when there are no tool calls). Intermediate done
                                // events from tool-call turns are suppressed to avoid prematurely
                                // finalizing the streaming bubble on the frontend.
                                final_usage = Some((usage.prompt_tokens, usage.completion_tokens, usage.total_tokens));
                            }
                            Err(e) => {
                                let _ = ws_sender
                                    .send(Message::Text(
                                        serde_json::json!({
                                            "type": "error",
                                            "message": format!("Stream error: {}", e)
                                        })
                                            .to_string()
                                            .into(),
                                    ))
                                    .await;
                                had_error = true;
                                break;
                            }
                        }
                    }

                    if had_error {
                        break;
                    }

                    // Build tool calls from accumulated deltas
                    let tool_calls: Vec<ToolCall> = current_tool_args
                        .into_iter()
                        .map(|(_, (call_id, func_name, arguments))| ToolCall {
                            id: call_id,
                            r#type: "function".to_string(),
                            function: jcowork_llm::provider::FunctionCall {
                                name: func_name,
                                arguments,
                            },
                        })
                        .collect();

                    // Log LLM request/response
                    let llm_duration_ms = llm_start.elapsed().as_millis() as u64;
                    let tool_names: Vec<String> = tool_calls.iter().map(|tc| tc.function.name.clone()).collect();
                    let log_entry = LogEntry::LlmRequest {
                        timestamp: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                        user_id: user_id.clone(),
                        provider: provider_name,
                        model: model_str.to_string(),
                        duration_ms: llm_duration_ms,
                        input: llm_input,
                        output: build_llm_output(
                            &assistant_content,
                            tool_calls.len(),
                            tool_names,
                            final_usage,
                        ),
                    };
                    let lw = log_writer.clone();
                    tokio::spawn(async move { lw.write(&log_entry).await });

                    // Add assistant message to history
                    history.push(ChatMessage {
                        role: "assistant".to_string(),
                        content: assistant_content,
                        tool_calls: if tool_calls.is_empty() { None } else { Some(tool_calls.clone()) },
                        tool_call_id: None,
                        reasoning_content: if reasoning_content.is_empty() { None } else { Some(reasoning_content) },
                    });

                    // If no tool calls, this is the final turn — send done to client
                    if tool_calls.is_empty() {
                        let (pt, ct, tt) = final_usage.unwrap_or((0, 0, 0));
                        let _ = ws_sender
                            .send(Message::Text(
                                serde_json::json!({
                                    "type": "done",
                                    "usage": {
                                        "prompt_tokens": pt,
                                        "completion_tokens": ct,
                                        "total_tokens": tt,
                                    }
                                })
                                    .to_string()
                                    .into(),
                            ))
                            .await;
                        break;
                    }

                    // Dispatch tool calls (skip empty-argument calls — some LLMs emit ghost tool calls)
                    for tc in &tool_calls {
                        if tc.function.arguments.trim().is_empty() {
                            tracing::debug!(tool = %tc.function.name, call_id = %tc.id, "Skipping tool call with empty arguments");
                            // Add error result to history so the LLM sees the feedback
                            history.push(ChatMessage {
                                role: "tool".to_string(),
                                content: "Error: tool call had empty arguments, skipped".to_string(),
                                tool_calls: None,
                                tool_call_id: Some(tc.id.clone()),
                                reasoning_content: None,
                            });
                            continue;
                        }

                        let tool_start = std::time::Instant::now();
                        let result = tool_registry
                            .dispatch(&tc.function.name, &tc.function.arguments, &tool_ctx)
                            .await;

                        let result_str = match result {
                            Ok(r) => r,
                            Err(e) => format!("Error: {}", e),
                        };

                        let tool_duration_ms = tool_start.elapsed().as_millis() as u64;

                        let _ = ws_sender
                            .send(Message::Text(
                                serde_json::json!({
                                    "type": "tool_call_end",
                                    "name": tc.function.name,
                                    "result": result_str.clone()
                                })
                                    .to_string()
                                    .into(),
                            ))
                            .await;

                        // Log tool call
                        let tool_log = ToolCallEntry::new(&user_id, &tc.function.name)
                            .into_log_entry_with(&tc.function.arguments, &result_str, tool_duration_ms);
                        let lw = log_writer.clone();
                        tokio::spawn(async move { lw.write(&tool_log).await });

                        // Add tool result to history
                        history.push(ChatMessage {
                            role: "tool".to_string(),
                            content: result_str,
                            tool_calls: None,
                            tool_call_id: Some(tc.id.clone()),
                            reasoning_content: None,
                        });
                    }

                    // Loop continues — LLM will see tool results and respond
                }
                    }
                    Some(Ok(Message::Close(_))) => break,
                    Some(Err(e)) => {
                        tracing::warn!(user_id = %user_id, err = %e, "WebSocket error");
                        break;
                    }
                    Some(Ok(_)) => {} // Ping/Pong/Binary ignored
                    None => break, // WS stream ended
                }
            }
            // Reminder notification from CronScheduler
            reminder = reminder_rx.recv() => {
                if let Ok(reminder) = reminder {
                    if reminder.user_id == user_id_for_reminder {
                        let _ = ws_sender
                            .send(Message::Text(
                                serde_json::to_string(&WsOutput::Reminder {
                                    id: reminder.id.clone(),
                                    message: reminder.message.clone(),
                                    fire_at: reminder.fire_at.clone(),
                                })
                                .unwrap()
                                .into(),
                            ))
                            .await;
                    }
                }
            }
        }
    }
}

/// Build the skill prompt block from enabled skills.
/// Loads enabled skill IDs from memory, then fetches content from builtin or user skills.
/// Returns (prompt_string, set_of_enabled_skill_ids).
async fn build_skill_prompt(memory_manager: &MemoryManager, skill_manager: &SkillManager, user_id: &str) -> (String, std::collections::HashSet<String>) {
    // Get enabled skill IDs
    let enabled_ids: std::collections::HashSet<String> = match memory_manager.recall_all(user_id).await {
        Ok(entries) => entries
            .into_iter()
            .filter(|e| e.category == "skill_enabled")
            .map(|e| e.content)
            .collect(),
        Err(_) => return (String::new(), std::collections::HashSet::new()),
    };

    if enabled_ids.is_empty() {
        return (String::new(), enabled_ids);
    }

    let mut blocks: Vec<String> = Vec::new();

    // Check built-in skills
    for s in builtin_skills() {
        if enabled_ids.contains(s.id) {
            blocks.push(s.content.to_string());
        }
    }

    // Check user skills
    if let Ok(user_skills) = skill_manager.list(user_id).await {
        for s in user_skills {
            if enabled_ids.contains(&s.id) {
                blocks.push(s.content);
            }
        }
    }

    if blocks.is_empty() {
        return (String::new(), enabled_ids);
    }

    (format!("\n\n---\n\n## Active Skills\n\n{}", blocks.join("\n\n---\n\n")), enabled_ids)
}

/// Load the user's custom agent identity from memory.
async fn load_agent_identity(memory_manager: &MemoryManager, user_id: &str) -> Option<String> {
    match memory_manager.recall_all(user_id).await {
        Ok(entries) => entries
            .into_iter()
            .find(|e| e.category == "agent_identity")
            .map(|e| e.content),
        Err(_) => None,
    }
}

/// Build a system context message containing the user's active reminders and cron jobs.
/// Returns None if there are no active reminders or cron jobs.
fn build_reminder_context_msg(reminders: &[Reminder], cron_jobs: &[CronJob]) -> Option<ChatMessage> {
    if reminders.is_empty() && cron_jobs.is_empty() {
        return None;
    }
    let mut parts = Vec::new();
    if !reminders.is_empty() {
        let lines: Vec<String> = reminders.iter().map(|r| format!("- [id: {}] \"{}\" (triggers at: {})", r.id, r.message, r.fire_at)).collect();
        parts.push(format!("User's active reminders:\n{}", lines.join("\n")));  
    }
    if !cron_jobs.is_empty() {
        let lines: Vec<String> = cron_jobs.iter().map(|cj| format!("- [id: {}] \"{}\" (Schedule: {})", cj.id, cj.prompt, cj.schedule)).collect();
        parts.push(format!("User's active cron jobs:\n{}", lines.join("\n")));  
    }
    Some(ChatMessage {
        role: "system".to_string(),
        content: parts.join("\n\n"),
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
    })
}



/// Build the system prompt that instructs the LLM about its capabilities.
/// If a custom identity is provided, it replaces the default "You are Jcowork Agent" prefix.
/// If skill_prompt is non-empty, it is appended at the end.
fn build_system_prompt_with_identity(custom_identity: Option<&str>, skill_prompt: &str) -> String {
    let now = chrono::Local::now();
    let current_time = now.format("%Y年%m月%d日 %H时%M分%S秒 (%:z)").to_string();

    let identity = match custom_identity {
        Some(id) if !id.trim().is_empty() => id.to_string(),
        _ => "You are Jcowork Agent, an intelligent AI assistant.".to_string(),
    };

    format!(
r#"{identity} You have the following tools available:

**reminder_add** — Set a one-time reminder. Use this when the user asks to set an alarm, reminder, or notification at a specific time.
  Parameters: fire_at (ISO 8601 datetime, e.g., "2026-05-15T11:41:00+08:00"), message (the reminder text)

**reminder_list** — List all active reminders for the current user.

**reminder_remove** — Remove a reminder by ID.

**cron_add** — Schedule a recurring task using cron syntax.

**cron_list** — List cron jobs.

**cron_remove** — Remove a cron job.

**memory_save** — Save a durable fact or life event to persistent memory.
  Parameters: content (declarative statement), category ('life_event' | 'preference' | 'environment' | 'convention' | 'person' | 'general')
  Returns: the saved entry including its `id` (UUID) — keep this ID in mind for possible later update.
  IMPORTANT: Call memory_save only ONCE per piece of information. Combine all related details into a single call. Do NOT make multiple memory_save calls for the same fact.

**memory_update** — Update an existing memory entry with new or enriched content.
  Parameters: id (UUID from memory_save), content (full updated text), category (optional)

**memory_recall** — Recall all saved memories (includes entry IDs).

**memory_search** — Search memories by keyword (includes entry IDs).
  Parameters: query (search terms), limit (max results, default 5)

When to use memory:
- The user explicitly tells you to remember something
- The user shares a preference or fact useful in future conversations
- **The user casually mentions a daily life event** — proactively save it with category='life_event':
  - Dropping off / picking up kids (e.g., "送孩子上学了" → save "2026-05-21 08:30 送孩子去学校")
  - Meals with someone (e.g., "和张总吃了个饭" → save "2026-05-21 午饭 和张总在[地点]吃饭")
  - Going somewhere / doing something (e.g., "去了趟医院" → save "2026-05-21 去医院看病")
  - Completing important tasks or meetings
  Always include the current date/time in the content for life events. Ask for missing details (who, where) only if the event seems significant.
- DO NOT save purely conversational context with no future value
- **CRITICAL: Call memory_save only ONCE per fact or event.** If a user mentions multiple related details (e.g., name + family info, preference + condition), combine them into a SINGLE memory_save call. NEVER make duplicate or parallel memory_save calls for the same information.

Life event memory rules:
- **Never mention the saving action in your reply.** Do NOT say things like "已记录"、"我帮你记下来了"、"已保存" etc. Just respond naturally to what the user said.
- **If the event is in the future** (e.g., "明天要去开会", "下周送孩子去夏令营"), after saving it ask: "需要到时提醒你吗？" — if the user says yes, use reminder_add or cron_add to set a reminder.
- **If the conversation continues on the same topic** and adds new details (location, people, outcome etc.), call memory_update with the original entry's id to enrich the content rather than saving a duplicate.

When the user asks you to set a reminder or alarm:
1. Parse the time they mentioned. If they say "11:41", assume today at 11:41 in the Asia/Shanghai timezone (UTC+8).
2. If the time has already passed today, use tomorrow's date.
3. Call the reminder_add tool with the full ISO 8601 datetime and the reminder message.
4. Confirm to the user that the reminder has been set.

今天是 {current_time}

IMPORTANT: When the user asks to set a reminder or alarm, DO NOT give instructions on how to use their phone's clock app. Instead, USE the reminder_add tool to actually set the reminder in the system.{skill_prompt}"#,
        identity = identity,
        current_time = current_time,
        skill_prompt = skill_prompt
    )
}
