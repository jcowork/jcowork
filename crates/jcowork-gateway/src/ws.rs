//! WebSocket handler for real-time agent communication.

use axum::extract::ws::{Message, WebSocket};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::RwLock;

use jcowork_agent::r#loop as agent_loop;
use jcowork_agent::r#loop::{AgentOutputSink, AgentTurnOptions, AgentTurnResult};
use jcowork_cron::CronScheduler;
use jcowork_llm::LlmRouter;
use jcowork_logs::LogWriter;
use jcowork_memory::MemoryManager;
use jcowork_skills::SkillManager;
use jcowork_tools::base::ToolContext;
use jcowork_tools::cron::{
    CronAddTool, CronListTool, CronRemoveTool, ReminderAddTool, ReminderListTool,
    ReminderRemoveTool,
};
use jcowork_tools::bing_search::WebSearchTool;
use jcowork_tools::doc_retrieve::{DocContentTool, DocRetrieveTool};
use jcowork_tools::doc_search::DocListTool;
use jcowork_tools::excel_db::ExcelDbTool;
use jcowork_tools::file_ops::{
    DirCreateTool, DirListTool, FileCopyTool, FileDeleteTool, FileInfoTool, FileListTool,
    FileMoveTool, FileReadTool, FileSearchTool, FileWriteTool,
};
use jcowork_tools::memory::{
    MemoryRecallTool, MemorySaveTool, MemorySearchTool, MemoryUpdateTool,
};
use jcowork_tools::pdf_parse::PdfParseTool;
use jcowork_tools::registry::ToolRegistry;
use jcowork_tools::shell::ShellTool;

use crate::session::SessionManager;

/// Incoming WebSocket message from client.
#[derive(Debug, Deserialize)]
pub struct WsInput {
    pub session_id: Option<String>,
    #[serde(rename = "type")]
    pub msg_type: Option<String>,
    pub content: Option<String>,
    pub model: Option<String>,
    pub context_documents: Option<Vec<ContextDocument>>,
    pub history: Option<Vec<HistoryMessage>>,
}

/// A reference document provided as context for a chat message.
#[derive(Debug, Deserialize)]
pub struct ContextDocument {
    pub name: String,
    pub path: Option<String>,
    pub content: String,
}

/// A historical message sent by the client to restore conversation context.
#[derive(Debug, Deserialize)]
pub struct HistoryMessage {
    pub role: String,
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
    registry.register(Arc::new(ReminderAddTool { scheduler: scheduler.clone() }));
    registry.register(Arc::new(ReminderListTool { scheduler: scheduler.clone() }));
    registry.register(Arc::new(ReminderRemoveTool { scheduler: scheduler.clone() }));
    registry.register(Arc::new(CronAddTool { scheduler: scheduler.clone() }));
    registry.register(Arc::new(CronListTool { scheduler: scheduler.clone() }));
    registry.register(Arc::new(CronRemoveTool { scheduler }));
    registry.register(Arc::new(MemorySaveTool { manager: memory_manager.clone() }));
    registry.register(Arc::new(MemoryUpdateTool { manager: memory_manager.clone() }));
    registry.register(Arc::new(MemoryRecallTool { manager: memory_manager.clone() }));
    registry.register(Arc::new(MemorySearchTool { manager: memory_manager }));
    registry.register(Arc::new(PdfParseTool::default()));
    registry.register(Arc::new(WebSearchTool::default().with_log_writer(log_writer)));
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
    registry.register(Arc::new(DocListTool));
    registry.register(Arc::new(DocRetrieveTool));
    registry.register(Arc::new(DocContentTool));
    registry.register(Arc::new(ExcelDbTool));
    registry.register(Arc::new(ShellTool::new(120)));
    Arc::new(registry)
}

// ─── WebSocket output sink ────────────────────────────────────────────

/// WebSocket output sink — implements AgentOutputSink to stream agent
/// events to the browser as JSON WebSocket messages.
struct WsSink<'a> {
    ws_sender: &'a mut futures::stream::SplitSink<WebSocket, Message>,
}

impl<'a> AgentOutputSink for WsSink<'a> {
    fn on_text_delta<'b>(&'b mut self, text: &'b str) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'b>> {
        Box::pin(async move {
            let _ = SinkExt::send(&mut self.ws_sender, Message::Text(
                serde_json::json!({"type": "text_delta", "content": text})
                    .to_string().into(),
            )).await;
        })
    }

    fn on_tool_call_start<'b>(&'b mut self, name: &'b str, call_id: &'b str, arguments: &'b str) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'b>> {
        let name = name.to_string();
        let call_id = call_id.to_string();
        let arguments = arguments.to_string();
        Box::pin(async move {
            let _ = SinkExt::send(&mut self.ws_sender, Message::Text(
                serde_json::json!({"type": "tool_call_start", "name": name, "call_id": call_id, "arguments": arguments})
                    .to_string().into(),
            )).await;
        })
    }

    fn on_tool_call_end<'b>(&'b mut self, name: &'b str, result: &'b str) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'b>> {
        let name = name.to_string();
        let result = result.to_string();
        Box::pin(async move {
            let _ = SinkExt::send(&mut self.ws_sender, Message::Text(
                serde_json::json!({"type": "tool_call_end", "name": name, "result": result})
                    .to_string().into(),
            )).await;
        })
    }

    fn on_done<'b>(&'b mut self, usage: Option<(i32, i32, i32)>) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'b>> {
        Box::pin(async move {
            let (pt, ct, tt) = usage.unwrap_or((0, 0, 0));
            let _ = SinkExt::send(&mut self.ws_sender, Message::Text(
                serde_json::json!({"type": "done", "usage": {"prompt_tokens": pt, "completion_tokens": ct, "total_tokens": tt}})
                    .to_string().into(),
            )).await;
        })
    }

    fn on_error<'b>(&'b mut self, message: &'b str) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'b>> {
        let message = message.to_string();
        Box::pin(async move {
            let _ = SinkExt::send(&mut self.ws_sender, Message::Text(
                serde_json::json!({"type": "error", "message": message})
                    .to_string().into(),
            )).await;
        })
    }

    fn on_status<'b>(&'b mut self, message: &'b str) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'b>> {
        let message = message.to_string();
        Box::pin(async move {
            let _ = SinkExt::send(&mut self.ws_sender, Message::Text(
                serde_json::json!({"type": "status", "message": message})
                    .to_string().into(),
            )).await;
        })
    }
}

// ─── WebSocket handler ────────────────────────────────────────────────

/// Handle a WebSocket connection for a specific user.
pub async fn ws_handler(
    ws: WebSocket,
    user_id: String,
    session_manager: Arc<SessionManager>,
    llm_router: Arc<RwLock<LlmRouter>>,
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
    let custom_identity = agent_loop::load_agent_identity(&memory_manager, &user_id).await;

    // Load enabled skills and build skill prompt blocks
    let (skill_prompt, enabled_skill_ids) =
        agent_loop::build_skill_prompt(&memory_manager, &skill_manager, &user_id).await;

    // Conversation history for this connection
    let mut history: Vec<jcowork_llm::provider::ChatMessage> = Vec::new();

    // System prompt
    let system_prompt =
        agent_loop::build_system_prompt_with_identity(custom_identity.as_deref(), &skill_prompt);
    history.push(jcowork_llm::provider::ChatMessage {
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
            // ── Incoming WebSocket messages from client ──
            msg = ws_receiver.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        // Parse incoming message
                        let input: WsInput = match serde_json::from_str(&text) {
                            Ok(v) => v,
                            Err(_) => {
                                let _ = ws_sender.send(Message::Text(
                                    serde_json::json!({"type": "error", "message": "Invalid JSON"})
                                        .to_string().into(),
                                )).await;
                                continue;
                            }
                        };

                        // Handle stop signal
                        if input.msg_type.as_deref() == Some("stop") {
                            tracing::info!(user_id = %user_id, "Received stop signal");
                            let _ = ws_sender.send(Message::Text(
                                serde_json::json!({"type": "stopped"}).to_string().into(),
                            )).await;
                            continue;
                        }

                        // Handle history restore: replace connection history with
                        // the client's persisted conversation (keep system prompt)
                        if input.msg_type.as_deref() == Some("load_history") {
                            let system = history.first().cloned();
                            history.clear();
                            if let Some(sys) = system {
                                history.push(sys);
                            }
                            if let Some(msgs) = input.history {
                                for m in msgs {
                                    if m.role != "user" && m.role != "assistant" {
                                        continue;
                                    }
                                    if m.content.trim().is_empty() {
                                        continue;
                                    }
                                    history.push(jcowork_llm::provider::ChatMessage {
                                        role: m.role,
                                        content: m.content,
                                        tool_calls: None,
                                        tool_call_id: None,
                                        reasoning_content: None,
                                    });
                                }
                            }
                            tracing::info!(user_id = %user_id, history_len = history.len(), "Conversation history restored");
                            continue;
                        }

                        let user_content = input.content.clone().unwrap_or_default();

                        // Status: message received
                        let _ = ws_sender.send(Message::Text(
                            serde_json::json!({"type": "status", "message": "📨 收到消息，正在处理..."})
                                .to_string().into(),
                        )).await;

                        // Reset system prompt (in case previous message had docs appended)
                        if !history.is_empty() {
                            history[0].content = system_prompt.clone();
                        }

                        // Inject context documents into system prompt
                        if let Some(docs) = &input.context_documents {
                            if !docs.is_empty() && !history.is_empty() {
                                let doc_count = docs.len();
                                let doc_names: Vec<&str> = docs.iter().map(|d| d.name.as_str()).collect();
                                let _ = ws_sender.send(Message::Text(
                                    serde_json::json!({"type": "status", "message": format!("📄 已加载 {} 个文档: {}", doc_count, doc_names.join(", "))})
                                        .to_string().into(),
                                )).await;

                                let doc_tuples: Vec<(String, Option<String>, String)> = docs.iter()
                                    .map(|d| (d.name.clone(), d.path.clone(), d.content.clone()))
                                    .collect();
                                agent_loop::inject_documents_to_system_prompt(&mut history, &doc_tuples);
                            }
                        }

                        // Inject Excel analysis guidance
                        let has_excel_docs = input.context_documents.as_ref().map(|docs| {
                            docs.iter().any(|doc| {
                                doc.name.ends_with(".xlsx") || doc.name.ends_with(".xls")
                                || doc.path.as_deref().map(|p| p.ends_with(".xlsx") || p.ends_with(".xls")).unwrap_or(false)
                            })
                        }).unwrap_or(false);
                        if let Some(extra_guidance) = agent_loop::build_excel_analysis_guidance(&user_content, has_excel_docs) {
                            if !history.is_empty() {
                                history[0].content.push_str(&extra_guidance);
                            }
                        }

                        // Add user message to history
                        history.push(jcowork_llm::provider::ChatMessage {
                            role: "user".to_string(),
                            content: user_content.clone(),
                            tool_calls: None,
                            tool_call_id: None,
                            reasoning_content: None,
                        });

                        // Resolve model
                        let model_str = input.model.as_deref().unwrap_or(&default_model);
                        let provider = {
                            let router = llm_router.read().unwrap();
                            router.get_provider(model_str)
                        };
                        let provider = match provider {
                            Ok(p) => p,
                            Err(e) => {
                                let _ = ws_sender.send(Message::Text(
                                    serde_json::json!({"type": "error", "message": e.to_string()})
                                        .to_string().into(),
                                )).await;
                                history.pop();
                                continue;
                            }
                        };

                        // Filter tools by enabled skills
                        let tools = agent_loop::filter_tools_by_skill(&tool_registry.all_schemas(), &enabled_skill_ids);

                        // Compute per-user workspace root
                        let workspace_root = format!("{}/{}/workspace", data_dir, user_id);
                        let _ = tokio::fs::create_dir_all(&workspace_root).await;
                        let tool_ctx = ToolContext {
                            user_id: user_id.clone(),
                            workspace_root,
                        };

                        // Fetch active reminders/cron jobs for context injection
                        let active_reminders = cron_scheduler.list_reminders(&user_id).await;
                        let active_cron_jobs = cron_scheduler.list_cron_jobs(&user_id).await;
                        let reminder_ctx_msg = agent_loop::build_reminder_context_msg(&active_reminders, &active_cron_jobs);

                        // ── Run agent turn via shared run_turn() ──
                        let mut sink = WsSink {
                            ws_sender: &mut ws_sender,
                        };

                        let result: AgentTurnResult = agent_loop::run_turn(AgentTurnOptions {
                            history: &mut history,
                            tools: &tools,
                            provider,
                            tool_registry: tool_registry.clone(),
                            tool_ctx: &tool_ctx,
                            pre_context: reminder_ctx_msg.as_ref(),
                            max_turns: 10,
                            llm_timeout_secs: 60,
                            stream_timeout_secs: 120,
                            tool_timeout_secs: 30,
                            output: &mut sink,
                            user_id: &user_id,
                            model: model_str,
                            log_writer: Some(log_writer.clone()),
                        }).await;

                        // Fallback if run_turn didn't send a done event
                        if !result.completed {
                            let fallback_message = if result.turns_used > 0 {
                                format!(
                                    "我已完成多轮工具探查，但还没来得及生成最终结论。你可以基于当前结果继续追问，或让我直接根据最近一次工具调用结果做总结。"
                                )
                            } else {
                                "我已完成处理流程，但模型没有产出最终文本回答。请直接重试一次，或让我基于当前已获取的数据继续总结。".to_string()
                            };
                            let _ = ws_sender.send(Message::Text(
                                serde_json::json!({"type": "text_delta", "content": fallback_message})
                                    .to_string().into(),
                            )).await;
                            let _ = ws_sender.send(Message::Text(
                                serde_json::json!({"type": "done", "usage": {"prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0}})
                                    .to_string().into(),
                            )).await;
                        }
                    }
                    Some(Ok(Message::Close(_))) => break,
                    Some(Err(e)) => {
                        tracing::warn!(user_id = %user_id, err = %e, "WebSocket error");
                        break;
                    }
                    Some(Ok(_)) => {}
                    None => break,
                }
            }
            // ── Reminder notification from CronScheduler ──
            reminder = reminder_rx.recv() => {
                if let Ok(reminder) = reminder {
                    if reminder.user_id == user_id_for_reminder {
                        // Send reminder notification to client
                        let _ = ws_sender.send(Message::Text(
                            serde_json::to_string(&WsOutput::Reminder {
                                id: reminder.id.clone(),
                                message: reminder.message.clone(),
                                fire_at: reminder.fire_at.clone(),
                            }).unwrap().into(),
                        )).await;

                        // Cron-job reminders are executed by the background cron executor.
                        // Only execute one-time reminder actions here (no cron_job_id).
                        if reminder.cron_job_id.is_none() {
                            if let Some(action) = &reminder.action {
                                tracing::info!(action = %action, "Executing reminder action");

                                history.push(jcowork_llm::provider::ChatMessage {
                                    role: "user".to_string(),
                                    content: action.clone(),
                                    tool_calls: None,
                                    tool_call_id: None,
                                    reasoning_content: None,
                                });

                                let provider = {
                                    let router = llm_router.read().unwrap();
                                    router.get_provider(&default_model)
                                };
                                if let Ok(provider) = provider {
                                    let tools = agent_loop::filter_tools_by_skill(
                                        &tool_registry.all_schemas(), &enabled_skill_ids,
                                    );
                                    let workspace_root = format!("{}/{}/workspace", data_dir, user_id);
                                    let _ = tokio::fs::create_dir_all(&workspace_root).await;
                                    let tool_ctx = ToolContext {
                                        user_id: user_id.clone(),
                                        workspace_root,
                                    };
                                    let mut sink = WsSink {
                                        ws_sender: &mut ws_sender,
                                    };

                                    let _ = agent_loop::run_turn(AgentTurnOptions {
                                        history: &mut history,
                                        tools: &tools,
                                        provider,
                                        tool_registry: tool_registry.clone(),
                                        tool_ctx: &tool_ctx,
                                        pre_context: None,
                                        max_turns: 5,
                                        llm_timeout_secs: 60,
                                        stream_timeout_secs: 120,
                                        tool_timeout_secs: 30,
                                        output: &mut sink,
                                        user_id: &user_id,
                                        model: &default_model,
                                        log_writer: Some(log_writer.clone()),
                                    }).await;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
