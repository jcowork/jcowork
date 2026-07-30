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
use jcowork_tools::doc_search::{DocListTool, DocSearchTool};
use jcowork_tools::excel_db::ExcelDbTool;
use jcowork_tools::file_ops::{FileReadTool, FileWriteTool, FileListTool, FileDeleteTool, FileMoveTool, FileCopyTool, FileSearchTool, DirCreateTool, DirListTool, FileInfoTool};
use jcowork_tools::memory::{MemorySaveTool, MemoryUpdateTool, MemoryRecallTool, MemorySearchTool};
use jcowork_tools::pdf_parse::PdfParseTool;
use jcowork_tools::report_search::{ReportListCompaniesTool, ReportSearchTool};
use jcowork_tools::registry::ToolRegistry;
use jcowork_tools::shell::ShellTool;

use crate::session::SessionManager;

/// Incoming WebSocket message from client.
#[derive(Debug, Deserialize)]
pub struct WsInput {
    pub session_id: Option<String>,
    pub content: String,
    /// Model string in "provider:model" format. Defaults to server default if not provided.
    pub model: Option<String>,
    /// Optional context documents (workspace files or uploaded PDFs) to include as reference.
    pub context_documents: Option<Vec<ContextDocument>>,
}

/// A reference document provided as context for a chat message.
#[derive(Debug, Deserialize)]
pub struct ContextDocument {
    pub name: String,
    pub path: Option<String>,
    pub content: String,
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
pub fn build_tool_registry(
    scheduler: Arc<CronScheduler>,
    memory_manager: Arc<MemoryManager>,
    log_writer: Arc<LogWriter>,
) -> Arc<ToolRegistry> {
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
    // Web search tool with log writer
    registry.register(Arc::new(WebSearchTool::default().with_log_writer(log_writer)));
    // Report search tools
    registry.register(Arc::new(ReportSearchTool::default()));
    registry.register(Arc::new(ReportListCompaniesTool::default()));
    // File operations tools (skill-gated behind builtin:code_engineer)
    registry.register(Arc::new(FileReadTool));
    registry.register(Arc::new(FileWriteTool));
    registry.register(Arc::new(FileListTool));
    registry.register(Arc::new(FileDeleteTool));
    registry.register(Arc::new(FileMoveTool));
    registry.register(Arc::new(FileCopyTool));
    registry.register(Arc::new(FileSearchTool));
    registry.register(Arc::new(DirCreateTool));
    registry.register(Arc::new(DirListTool));
    registry.register(Arc::new(FileInfoTool));
    // Document search tools (workspace index)
    registry.register(Arc::new(DocSearchTool));
    registry.register(Arc::new(DocListTool));
    // Excel database CRUD tool (skill-gated behind builtin:excel_data)
    registry.register(Arc::new(ExcelDbTool));
    // Shell tool (skill-gated behind builtin:code_engineer)
    registry.register(Arc::new(ShellTool::new(120)));
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
    data_dir: String,
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
        content: system_prompt.clone(),
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

                // Send status: message received
                let _ = ws_sender
                    .send(Message::Text(
                        serde_json::json!({"type": "status", "message": "📨 收到消息，正在处理..."})
                            .to_string()
                            .into(),
                    ))
                    .await;

                // Add user message to history
                // If context documents are provided, inject them DIRECTLY into the system prompt
                // so the LLM sees them as part of its core instructions.
                // First, reset system prompt to original (in case previous message had docs appended)
                if !history.is_empty() {
                    history[0].content = system_prompt.clone();
                }
                if let Some(docs) = &input.context_documents {
                    if !docs.is_empty() && !history.is_empty() {
                        let doc_count = docs.len();
                        let doc_names: Vec<&str> = docs.iter().map(|d| d.name.as_str()).collect();
                        // Send status: documents loaded
                        let _ = ws_sender
                            .send(Message::Text(
                                serde_json::json!({"type": "status", "message": format!("📄 已加载 {} 个文档: {}", doc_count, doc_names.join(", "))})
                                    .to_string()
                                    .into(),
                            ))
                            .await;

                        let mut parts = Vec::new();
                        for doc in docs {
                            let path_info = doc.path.as_deref().unwrap_or("");
                            // Truncate very long documents to avoid exceeding context window
                            let max_len = 12000;
                            let content = if doc.content.len() > max_len {
                                format!("{}\n\n[... content truncated, {} chars total ...]", &doc.content[..max_len], doc.content.len())
                            } else {
                                doc.content.clone()
                            };
                            parts.push(format!(
                                "=== {} (path: {}) ===\n{}\n=== END ===",
                                doc.name, path_info, content
                            ));
                        }
                        let doc_block = format!(
                            "\n\n## ATTACHED REFERENCE DOCUMENTS\n\
                             The user has attached document(s) to this conversation. \
                             Their FULL TEXT CONTENT is provided below — you ALREADY have all the information.\n\n\
                             ⚠️ CRITICAL RULES — YOU MUST FOLLOW THESE:\n\
                             1. The document content is RIGHT HERE below. You DO NOT need to read, parse, or access any files.\n\
                             2. ANSWER DIRECTLY using the content below. Do NOT call pdf_parse, file_read, dir_list, shell, or ANY tool to access these files.\n\
                             3. ONLY use external tools if the user explicitly asks for info BEYOND what's in the documents.\n\
                             4. If asked to modify a document, use file_write to save changes.\n\n\
                             {}\n",
                            parts.join("\n\n")
                        );
                        // Append to the system prompt (history[0])
                        history[0].content = format!("{}{}", history[0].content, doc_block);
                    }
                }

                if let Some(extra_guidance) = build_excel_analysis_guidance(&input) {
                   if !history.is_empty() {
                        history[0].content.push_str(&extra_guidance);
                    }
                }

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
                            "excel_db" => Some("builtin:excel_data"),
                            "file_read" | "file_write" | "file_list" | "file_delete" | "file_move" | "file_copy" | "file_search" | "dir_create" | "dir_list" | "file_info" | "shell" => Some("builtin:code_engineer"),
                            _ => None,
                        };
                        match skill_required {
                            Some(skill_id) => enabled_skill_ids.contains(skill_id),
                            None => true,
                        }
                    })
                    .collect();
                // Compute per-user workspace root and ensure it exists
                let workspace_root = format!("{}/{}/workspace", data_dir, user_id);
                let _ = tokio::fs::create_dir_all(&workspace_root).await;
                let tool_ctx = ToolContext {
                    user_id: user_id.clone(),
                    workspace_root,
                };

                // Fetch active reminders and cron jobs to inject as context
                let active_reminders = cron_scheduler.list_reminders(&user_id).await;
                let active_cron_jobs = cron_scheduler.list_cron_jobs(&user_id).await;
                let reminder_ctx_msg = build_reminder_context_msg(&active_reminders, &active_cron_jobs);

                let mut sent_done = false;
                let mut last_tool_summaries: Vec<String> = Vec::new();


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

                    // Call LLM with streaming (with timeout to prevent hanging on large contexts)
                    let llm_start = std::time::Instant::now();
                    let llm_input = build_llm_input(&effective_history.iter().map(|m| (m.role.as_str(), m.content.as_str())).collect::<Vec<_>>());
                    let provider_name = provider.name().to_string();

                    // Send status: LLM call starting
                    if _turn == 0 {
                        let _ = ws_sender
                            .send(Message::Text(
                                serde_json::json!({"type": "status", "message": format!("🤖 正在调用 {} ...", provider_name)})
                                    .to_string()
                                    .into(),
                            ))
                            .await;
                    } else {
                        let _ = ws_sender
                            .send(Message::Text(
                                serde_json::json!({"type": "status", "message": format!("🔄 工具调用完成，继续思考 (第{}轮)...", _turn + 1)})
                                    .to_string()
                                    .into(),
                            ))
                            .await;
                    }
                    
                    // Add timeout: if LLM doesn't respond within 60 seconds, abort
                    let stream_result = tokio::time::timeout(
                        std::time::Duration::from_secs(60),
                        provider.chat_stream(&effective_history, &tools)
                    ).await;
                    
                    let mut stream = match stream_result {
                        Ok(Ok(s)) => s,
                        Ok(Err(e)) => {
                            let _ = ws_sender
                                .send(Message::Text(
                                    serde_json::json!({"type": "error", "message": format!("LLM error: {}", e)})
                                        .to_string()
                                        .into(),
                                ))
                                .await;
                            break;
                        }
                        Err(_) => {
                            // Timeout - likely due to context being too large
                            let elapsed = llm_start.elapsed();
                            let _ = ws_sender
                                .send(Message::Text(
                                    serde_json::json!({
                                        "type": "error",
                                        "message": format!("LLM request timed out after {:.1}s. The attached document(s) may be too large for the model's context window. Try asking a more specific question or using a smaller document.", elapsed.as_secs_f64())
                                    })
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

                    loop {
                        let chunk = match tokio::time::timeout(
                            std::time::Duration::from_secs(120),
                            stream.next()
                        ).await {
                            Ok(Some(c)) => c,
                            Ok(None) => break, // Stream ended normally
                            Err(_) => {
                                // Stream timeout - LLM stopped responding mid-stream
                                let _ = ws_sender
                                    .send(Message::Text(
                                        serde_json::json!({
                                            "type": "error",
                                            "message": "LLM stream timed out. The response was too large or the connection was lost."
                                        })
                                            .to_string()
                                            .into(),
                                    ))
                                    .await;
                                had_error = true;
                                break;
                            }
                        };
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
                                    .or_insert_with(|| (call_id.clone(), String::new(), String::new()));
                                if !name.trim().is_empty() {
                                    entry.1 = name.clone();
                                }

                                entry.2.push_str(&args_delta);
                                
                                if !entry.1.trim().is_empty() && tool_call_started.insert(call_id.clone()) {
                                    let _ = ws_sender
                                        .send(Message::Text(
                                            serde_json::json!({
                                                "type": "tool_call_start",
                                                "name": entry.1.clone(),
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

                    last_tool_summaries = tool_calls
                        .iter()
                        .map(|tc| format!("{}({})", tc.function.name, tc.id))
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
                        sent_done = true;
                        break;
                    }

                    // Dispatch tool calls (skip empty-argument calls — some LLMs emit ghost tool calls)
                    for tc in &tool_calls {
                        if tc.function.name.trim().is_empty() {
                            tracing::warn!(call_id = %tc.id, arguments = %tc.function.arguments, "Skipping tool call with empty name");
                            history.push(ChatMessage {
                                role: "tool".to_string(),
                                content: "Error: tool call had empty name, skipped".to_string(),
                                tool_calls: None,
                                tool_call_id: Some(tc.id.clone()),
                                reasoning_content: None,
                            });
                            continue;
                        }

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

                if !sent_done {
                    let fallback_message = if !last_tool_summaries.is_empty() {
                        format!(
                            "我已完成多轮工具探查，但还没来得及生成最终结论。你可以基于当前结果继续追问，或让我直接根据最近一次工具调用结果做总结。最近一次涉及的工具调用：{}。",
                            last_tool_summaries.join("，")
                        )
                    } else {
                        "我已完成处理流程，但模型没有产出最终文本回答。请直接重试一次，或让我基于当前已获取的数据继续总结。".to_string()
                    };
                    let _ = ws_sender
                        .send(Message::Text(
                            serde_json::json!({"type": "text_delta", "content": fallback_message})
                                .to_string()
                                .into(),
                        ))
                        .await;
                    let _ = ws_sender
                        .send(Message::Text(
                            serde_json::json!({
                                "type": "done",
                                "usage": {
                                    "prompt_tokens": 0,
                                    "completion_tokens": 0,
                                    "total_tokens": 0,
                                }
                            })
                                .to_string()
                                .into(),
                        ))
                        .await;
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
                        // Send reminder notification to client
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

                        // If reminder has an action, execute it automatically
                        if let Some(action) = &reminder.action {
                            tracing::info!(action = %action, "Executing reminder action");

                            // Add the action as a user message to history
                            history.push(ChatMessage {
                                role: "user".to_string(),
                                content: action.clone(),
                                tool_calls: None,
                                tool_call_id: None,
                                reasoning_content: None,
                            });

                            // Get provider and tools for executing the action
                            let provider = match llm_router.get_provider(&default_model) {
                                Ok(p) => p,
                                Err(e) => {
                                    tracing::error!(err = %e, "Failed to get provider for reminder action");
                                    continue;
                                }
                            };

                            let tools: Vec<_> = tool_registry.all_schemas()
                                .into_iter()
                                .filter(|t| {
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

                            // Agent loop for reminder action - continue until no more tool calls
                            let max_turns = 5;
                            let mut final_usage: Option<(i32, i32, i32)> = None;
                            for _turn in 0..max_turns {
                                let llm_start = std::time::Instant::now();
                                let llm_input = build_llm_input(&history.iter().map(|m| (m.role.as_str(), m.content.as_str())).collect::<Vec<_>>());
                                let provider_name = provider.name().to_string();

                                let stream_result = tokio::time::timeout(
                                    std::time::Duration::from_secs(60),
                                    provider.chat_stream(&history, &tools)
                                ).await;
                                let mut stream = match stream_result {
                                    Ok(Ok(s)) => s,
                                    Ok(Err(e)) => {
                                        tracing::error!(err = %e, "Failed to start LLM stream for reminder action");
                                        break;
                                    }
                                    Err(_) => {
                                        tracing::error!("LLM stream timed out for reminder action");
                                        break;
                                    }
                                };

                                let mut assistant_content = String::new();
                                let mut reasoning_content = String::new();
                                let mut current_tool_args: HashMap<String, (String, String, String)> = HashMap::new();
                                let mut tool_call_started: std::collections::HashSet<String> = std::collections::HashSet::new();

                                loop {
                                    let chunk = match tokio::time::timeout(
                                        std::time::Duration::from_secs(120),
                                        stream.next()
                                    ).await {
                                        Ok(Some(c)) => c,
                                        Ok(None) => break,
                                        Err(_) => {
                                            tracing::error!("LLM stream timed out mid-response for reminder action");
                                            break;
                                        }
                                    };
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
                                        }
                                        Ok(StreamChunk::ToolCallDelta(call_id, name, args_delta)) => {
                                            let entry = current_tool_args
                                                .entry(call_id.clone())
                                                .or_insert_with(|| (call_id.clone(), String::new(), String::new()));
                                            if !name.trim().is_empty() {
                                                entry.1 = name.clone();
                                            }
                                            entry.2.push_str(&args_delta);
                                            if !entry.1.trim().is_empty() && tool_call_started.insert(call_id.clone()) {
                                                let _ = ws_sender
                                                    .send(Message::Text(
                                                        serde_json::json!({
                                                            "type": "tool_call_start",
                                                            "name": entry.1.clone(),
                                                            "call_id": call_id
                                                        })
                                                            .to_string()
                                                            .into(),
                                                    ))
                                                    .await;
                                            }
                                        }
                                        Ok(StreamChunk::Done(usage)) => {
                                            final_usage = Some((usage.prompt_tokens, usage.completion_tokens, usage.total_tokens));
                                        }
                                        Err(e) => {
                                            tracing::error!(err = %e, "Stream error during reminder action");
                                            break;
                                        }
                                    }
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

                                // Log LLM request/response for this turn
                                let llm_duration_ms = llm_start.elapsed().as_millis() as u64;
                                let tool_names: Vec<String> = tool_calls.iter().map(|tc| tc.function.name.clone()).collect();
                                let log_entry = LogEntry::LlmRequest {
                                    timestamp: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                                    user_id: user_id.clone(),
                                    provider: provider_name,
                                    model: default_model.clone(),
                                    duration_ms: llm_duration_ms,
                                    input: llm_input,
                                    output: build_llm_output(&assistant_content, tool_calls.len(), tool_names, final_usage),
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

                                // If no tool calls, we're done
                                if tool_calls.is_empty() {
                                    // Send done notification
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

                                // Handle tool calls
                                for tc in &tool_calls {
                                    if tc.function.name.trim().is_empty() {
                                        tracing::warn!(call_id = %tc.id, arguments = %tc.function.arguments, "Skipping tool call with empty name during reminder action");
                                        history.push(ChatMessage {
                                            role: "tool".to_string(),
                                            content: "Error: tool call had empty name, skipped".to_string(),
                                            tool_calls: None,
                                            tool_call_id: Some(tc.id.clone()),
                                            reasoning_content: None,
                                        });
                                        continue;
                                    }

                                    if tc.function.arguments.trim().is_empty() {
                                        tracing::debug!(tool = %tc.function.name, call_id = %tc.id, "Skipping tool call with empty arguments");
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

                                    history.push(ChatMessage {
                                        role: "tool".to_string(),
                                        content: result_str,
                                        tool_calls: None,
                                        tool_call_id: Some(tc.id.clone()),
                                        reasoning_content: None,
                                    });
                                }
                                // Loop continues - LLM will see tool results and respond
                            }
                        }
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

fn build_excel_analysis_guidance(input: &WsInput) -> Option<String> {
    let mentions_excel = input.content.contains("xlsx")
        || input.content.contains("xls")
        || input.content.contains("Excel")
        || input.content.contains("表格")
        || input.content.contains("工作表")
        || input.content.contains("query");

    let has_excel_context = input
        .context_documents
        .as_ref()
        .map(|docs| {
            docs.iter().any(|doc| {
                doc.name.ends_with(".xlsx")
                    || doc.name.ends_with(".xls")
                    || doc
                        .path
                        .as_deref()
                        .map(|p| p.ends_with(".xlsx") || p.ends_with(".xls"))
                        .unwrap_or(false)
            })
        })
        .unwrap_or(false);

    if !mentions_excel && !has_excel_context {
        return None;
    }

    Some(
        "\n\nExcel analysis guidance:\n- If the user asks about an uploaded Excel file, prefer using excel_db first. Start with action=\"list\" to find the imported database, then inspect schema if needed, then run focused SELECT queries.\n- Do not call file_list, file_search, or doc_list unless excel_db clearly shows the workbook/database is missing or you need to locate the source file after excel_db fails.\n- Avoid repeated exploratory tool loops. Once you have enough rows or aggregates to answer, stop calling tools and provide a concise conclusion.\n- For questions like '大家都在问什么' or query analysis, summarize recurring intents/themes, representative examples, and notable frequencies/patterns from the data instead of only dumping raw rows."
            .to_string(),
    )
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
        role: "user".to_string(),
        content: format!("[Context] {}", parts.join("\n\n")),
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
    })
}



/// Build the system prompt that instructs the LLM about its capabilities.
/// If a custom identity is provided, it replaces the default "You are Jcowork Agent" prefix.
/// If skill_prompt is non-empty, it is appended at the end.
pub fn build_system_prompt_with_identity(custom_identity: Option<&str>, skill_prompt: &str) -> String {
    let now = chrono::Local::now();
    let current_time = now.format("%Y年%m月%d日 %H时%M分%S秒 (%:z)").to_string();

    let identity = match custom_identity {
        Some(id) if !id.trim().is_empty() => id.to_string(),
        _ => "You are Jcowork Agent, an intelligent AI assistant.".to_string(),
    };

    format!(
r#"{identity} You have the following tools available:

**reminder_add** — Set a one-time reminder. Use this when the user asks to set an alarm, reminder, or notification at a specific time.
  Parameters: fire_at (北京时间，ISO 8601 格式，如 "2026-05-15T11:41:00+08:00"), message (the reminder text)

**reminder_list** — List all active reminders for the current user.

**reminder_remove** — Remove a reminder by ID.

**cron_add** — Schedule a recurring task using cron syntax.

**cron_list** — List cron jobs.

**cron_remove** — Remove a cron job.

**memory_save** — Save a durable fact or life event to persistent memory.
  Parameters: content (declarative statement), category ('life_event' | 'preference' | 'environment' | 'convention' | 'person' | 'habit' | 'general')
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
1. Parse the time they mentioned. If they say "11:41", assume today at 11:41 北京时间 (UTC+8)。所有时间均使用北京时间，fire_at 的时区偏移固定为 +08:00。
2. If the time has already passed today, use tomorrow's date.
3. Call the reminder_add tool with the full ISO 8601 datetime and the reminder message.
4. Only AFTER the tool returns success, confirm to the user that the reminder has been set.

CRITICAL REMINDER RULES:
- You MUST call the reminder_add tool to actually set reminders. NEVER claim a reminder is set without calling the tool.
- Do NOT describe reminders in text and pretend they are set. Text descriptions are NOT reminders.
- When setting multiple reminders, call reminder_add for EACH one separately (one tool call per reminder).
- After all reminder_add calls return success, briefly confirm the total count to the user.

今天是 {current_time}

IMPORTANT: When the user asks to set a reminder or alarm, DO NOT give instructions on how to use their phone's clock app. Instead, USE the reminder_add tool to actually set the reminder in the system. NEVER write out a list of reminders in text without calling the tool — that is a hallucination, not a real reminder.{skill_prompt}"#,
        identity = identity,
        current_time = current_time,
        skill_prompt = skill_prompt
    )
}
