//! WebSocket handler for real-time agent communication.

use axum::extract::ws::{Message, WebSocket};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

use jcowork_cron::CronScheduler;
use jcowork_llm::provider::{ChatMessage, StreamChunk, ToolCall};
use jcowork_llm::LlmRouter;
use jcowork_memory::MemoryManager;
use jcowork_tools::base::ToolContext;
use jcowork_tools::cron::{ReminderAddTool, ReminderListTool, ReminderRemoveTool, CronAddTool, CronListTool, CronRemoveTool};
use jcowork_tools::memory::{MemorySaveTool, MemoryRecallTool, MemorySearchTool};
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
    registry.register(Arc::new(MemoryRecallTool { manager: memory_manager.clone() }));
    registry.register(Arc::new(MemorySearchTool { manager: memory_manager }));
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
) {
    let (mut ws_sender, mut ws_receiver) = ws.split();

    // Get or create UserActor for this user
    let _actor = session_manager.get(&user_id);

    // Conversation history for this connection
    let mut history: Vec<ChatMessage> = Vec::new();

    // System prompt
    let system_prompt = build_system_prompt();
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

                // Get tool schemas
                let tools = tool_registry.all_schemas();
                let tool_ctx = ToolContext {
                    user_id: user_id.clone(),
                    workspace_root: String::new(),
                };

                // Agent loop: keep calling LLM until it stops making tool calls
                let max_turns = 10;
                for _turn in 0..max_turns {
                    // Call LLM with streaming
                    let stream_result = provider.chat_stream(&history, &tools).await;
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
                                let _ = ws_sender
                                    .send(Message::Text(
                                        serde_json::json!({
                                            "type": "done",
                                            "usage": {
                                                "prompt_tokens": usage.prompt_tokens,
                                                "completion_tokens": usage.completion_tokens,
                                                "total_tokens": usage.total_tokens,
                                            }
                                        })
                                            .to_string()
                                            .into(),
                                    ))
                                    .await;
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

                    // Add assistant message to history
                    history.push(ChatMessage {
                        role: "assistant".to_string(),
                        content: assistant_content,
                        tool_calls: if tool_calls.is_empty() { None } else { Some(tool_calls.clone()) },
                        tool_call_id: None,
                        reasoning_content: if reasoning_content.is_empty() { None } else { Some(reasoning_content) },
                    });

                    // If no tool calls, we're done
                    if tool_calls.is_empty() {
                        break;
                    }

                    // Dispatch tool calls
                    for tc in &tool_calls {
                        let result = tool_registry
                            .dispatch(&tc.function.name, &tc.function.arguments, &tool_ctx)
                            .await;

                        let result_str = match result {
                            Ok(r) => r,
                            Err(e) => format!("Error: {}", e),
                        };

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

/// Build the system prompt that instructs the LLM about its capabilities.
fn build_system_prompt() -> String {
    let now = chrono::Local::now();
    let current_time = now.format("%Y-%m-%d %H:%M:%S %:z").to_string();

    format!(
r#"You are Jcowork Agent, an intelligent AI assistant. You have the following tools available:

**reminder_add** — Set a one-time reminder. Use this when the user asks to set an alarm, reminder, or notification at a specific time.
  Parameters: fire_at (ISO 8601 datetime, e.g., "2026-05-15T11:41:00+08:00"), message (the reminder text)

**reminder_list** — List all active reminders for the current user.

**reminder_remove** — Remove a reminder by ID.

**cron_add** — Schedule a recurring task using cron syntax.

**cron_list** — List cron jobs.

**cron_remove** — Remove a cron job.

**memory_save** — Save a durable fact to persistent memory. Use for user preferences, environment details, and stable conventions.
  Parameters: content (the fact to save, as a declarative statement), category (e.g., 'preference', 'environment', 'convention')

**memory_recall** — Recall all saved memories.

**memory_search** — Search memories by keyword.
  Parameters: query (search terms), limit (max results, default 5)

When to use memory:
- The user explicitly tells you to remember something (e.g., "remember that I prefer dark mode")
- The user shares a preference or fact that would be useful in future conversations
- DO NOT save conversational context that is only relevant to the current chat

When the user asks you to set a reminder or alarm:
1. Parse the time they mentioned. If they say "11:41", assume today at 11:41 in the Asia/Shanghai timezone (UTC+8).
2. If the time has already passed today, use tomorrow's date.
3. Call the reminder_add tool with the full ISO 8601 datetime and the reminder message.
4. Confirm to the user that the reminder has been set.

Current date/time: {}

IMPORTANT: When the user asks to set a reminder or alarm, DO NOT give instructions on how to use their phone's clock app. Instead, USE the reminder_add tool to actually set the reminder in the system."#,
        current_time
    )
}
