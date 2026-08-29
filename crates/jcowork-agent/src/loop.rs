//! Agent Loop - core orchestrator for LLM calls, tool dispatch, and streaming.
//!
//! This module provides two levels of agent loop abstraction:
//!
//! 1. **`AgentLoop` struct** — a stateful, session-based agent loop (legacy, kept for
//!    backward compatibility). Manages its own message history and context engine.
//!
//! 2. **`run_turn()` function** — a stateless, transport-agnostic agent turn that
//!    accepts conversation history and an output sink. This is the **production**
//!    implementation used by both the WebSocket handler and the Feishu handler.
//!    It includes timeout protection, structured logging, skill-gated tool filtering,
//!    document context injection, and reminder/cron context — all features that were
//!    previously duplicated across ws.rs and feishu.rs.

use anyhow::Result;
use futures::StreamExt;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::mpsc;

use jcowork_cron::{CronJob, Reminder};
use jcowork_llm::provider::{
    ChatMessage, FunctionCall, LlmProvider, StreamChunk, ToolCall, ToolDefinition, Usage,
};
use jcowork_logs::{build_llm_input, build_llm_output, LogEntry, LogWriter, ToolCallEntry};
use jcowork_memory::MemoryManager;
use jcowork_skills::{builtin_skills, SkillManager};
use jcowork_tools::base::ToolContext;
use jcowork_tools::registry::ToolRegistry;

use crate::context::{Compressor, ContextEngine};
use crate::prompt::PromptBuilder;

// ─── Legacy types (kept for backward compatibility) ───────────────────

/// Message sent from WebSocket to AgentLoop.
#[derive(Debug, Clone)]
pub struct UserMessage {
    pub session_id: String,
    pub content: String,
}

/// Message sent from AgentLoop back to the client.
#[derive(Debug, Clone)]
pub enum AgentOutput {
    TextDelta(String),
    ToolCallStart { name: String, call_id: String },
    ToolCallEnd { name: String, result: String },
    Done { usage: Usage },
    Error(String),
}

/// Configuration for an agent instance.
#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub user_id: String,
    pub workspace_root: String,
    pub model: String,
    pub max_turns: usize,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            user_id: String::new(),
            workspace_root: String::new(),
            model: "openai:gpt-4o".to_string(),
            max_turns: 20,
        }
    }
}

/// The core agent loop (legacy stateful API).
///
/// Prefer [`run_turn`] for new code — it is the production implementation used
/// by both the WebSocket and Feishu handlers.
pub struct AgentLoop {
    config: AgentConfig,
    messages: Vec<ChatMessage>,
    tool_registry: Arc<ToolRegistry>,
    memory_manager: Arc<MemoryManager>,
    skill_manager: Arc<SkillManager>,
    llm_provider: Arc<dyn LlmProvider>,
    context_engine: Box<dyn ContextEngine>,
}

impl AgentLoop {
    pub fn new(
        config: AgentConfig,
        tool_registry: Arc<ToolRegistry>,
        memory_manager: Arc<MemoryManager>,
        skill_manager: Arc<SkillManager>,
        llm_provider: Arc<dyn LlmProvider>,
    ) -> Self {
        let context_engine = Box::new(Compressor::new(llm_provider.context_length()));
        Self {
            config,
            messages: Vec::new(),
            tool_registry,
            memory_manager,
            skill_manager,
            llm_provider,
            context_engine,
        }
    }

    pub async fn run(
        &mut self,
        user_message: &str,
        output_tx: mpsc::Sender<AgentOutput>,
    ) -> Result<()> {
        let memory_context = self
            .memory_manager
            .build_system_prompt(&self.config.user_id)
            .await;
        let skill_index = self
            .skill_manager
            .build_skill_index(&self.config.user_id)
            .await;
        let system_prompt = PromptBuilder::new()
            .memory_context(memory_context)
            .skill_index(skill_index)
            .build();

        if self.messages.is_empty() {
            self.messages.push(ChatMessage {
                role: "system".to_string(),
                content: system_prompt,
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            });
        } else {
            self.messages[0].content = system_prompt;
        }

        self.messages.push(ChatMessage {
            role: "user".to_string(),
            content: user_message.to_string(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        });

        let mut turns = 0;
        loop {
            turns += 1;
            if turns > self.config.max_turns {
                let _ = output_tx
                    .send(AgentOutput::Error("Max turns reached".to_string()))
                    .await;
                break;
            }

            if self
                .context_engine
                .should_compress(self.estimate_tokens())
            {
                self.messages = self
                    .context_engine
                    .compress(self.messages.clone(), self.estimate_tokens())
                    .await?;
            }

            let tools = self.tool_registry.all_schemas();
            let stream_result = self.llm_provider.chat_stream(&self.messages, &tools).await;
            let mut stream = match stream_result {
                Ok(s) => s,
                Err(e) => {
                    let _ = output_tx
                        .send(AgentOutput::Error(format!("LLM error: {}", e)))
                        .await;
                    break;
                }
            };

            let mut assistant_content = String::new();
            let mut reasoning_content = String::new();
            let mut tool_calls: Vec<ToolCall> = Vec::new();
            let mut current_tool_args: HashMap<String, (String, String, String)> = HashMap::new();

            while let Some(chunk) = stream.next().await {
                match chunk {
                    Ok(StreamChunk::Delta(text)) => {
                        assistant_content.push_str(&text);
                        let _ = output_tx.send(AgentOutput::TextDelta(text)).await;
                    }
                    Ok(StreamChunk::ReasoningDelta(reasoning)) => {
                        reasoning_content.push_str(&reasoning);
                    }
                    Ok(StreamChunk::ToolCallDelta(call_id, func_name, args_delta)) => {
                        let entry = current_tool_args.entry(call_id.clone()).or_insert_with(
                            || (call_id.clone(), func_name.clone(), String::new()),
                        );
                        entry.2.push_str(&args_delta);
                        let _ = output_tx
                            .send(AgentOutput::ToolCallStart {
                                name: func_name.clone(),
                                call_id: call_id.clone(),
                            })
                            .await;
                    }
                    Ok(StreamChunk::Done(usage)) => {
                        self.context_engine.update_from_response(&usage);
                        let _ = output_tx.send(AgentOutput::Done { usage }).await;
                    }
                    Err(e) => {
                        let _ = output_tx
                            .send(AgentOutput::Error(format!("Stream error: {}", e)))
                            .await;
                        break;
                    }
                }
            }

            for (_, (call_id, func_name, arguments)) in current_tool_args {
                tool_calls.push(ToolCall {
                    id: call_id,
                    r#type: "function".to_string(),
                    function: FunctionCall {
                        name: func_name,
                        arguments,
                    },
                });
            }

            self.messages.push(ChatMessage {
                role: "assistant".to_string(),
                content: assistant_content,
                tool_calls: if tool_calls.is_empty() {
                    None
                } else {
                    Some(tool_calls.clone())
                },
                tool_call_id: None,
                reasoning_content: if reasoning_content.is_empty() {
                    None
                } else {
                    Some(reasoning_content)
                },
            });

            if tool_calls.is_empty() {
                break;
            }

            let tool_ctx = ToolContext {
                user_id: self.config.user_id.clone(),
                workspace_root: self.config.workspace_root.clone(),
            };
            for tc in &tool_calls {
                let result = self
                    .tool_registry
                    .dispatch_isolated(&tc.function.name, &tc.function.arguments, &tool_ctx)
                    .await;
                let result_str = match result {
                    Ok(r) => r,
                    Err(e) => format!("Error: {}", e),
                };
                let _ = output_tx
                    .send(AgentOutput::ToolCallEnd {
                        name: tc.function.name.clone(),
                        result: result_str.clone(),
                    })
                    .await;
                self.messages.push(ChatMessage {
                    role: "tool".to_string(),
                    content: result_str,
                    tool_calls: None,
                    tool_call_id: Some(tc.id.clone()),
                    reasoning_content: None,
                });
            }
        }

        Ok(())
    }

    fn estimate_tokens(&self) -> i32 {
        let total_chars: usize = self.messages.iter().map(|m| m.content.len()).sum();
        (total_chars / 4) as i32
    }

    pub fn reset(&mut self) {
        self.messages.clear();
    }

    pub fn messages(&self) -> &[ChatMessage] {
        &self.messages
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Production agent turn — transport-agnostic, used by ws.rs & feishu.rs
// ═══════════════════════════════════════════════════════════════════════

/// Transport-agnostic output sink for agent events.
///
/// Implement this trait to bridge the agent loop to any transport
/// (WebSocket, HTTP, Feishu API, etc.).
pub trait AgentOutputSink {
    /// Stream a text delta from the LLM response.
    fn on_text_delta<'a>(&'a mut self, text: &'a str) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>>;

    /// A tool call is starting (after stream completes, with full arguments).
    fn on_tool_call_start<'a>(&'a mut self, name: &'a str, call_id: &'a str, arguments: &'a str) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>>;

    /// A tool call has completed with its result.
    fn on_tool_call_end<'a>(&'a mut self, name: &'a str, result: &'a str) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>>;

    /// The agent turn is done (no more tool calls).
    fn on_done<'a>(&'a mut self, usage: Option<(i32, i32, i32)>) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>>;

    /// An error occurred.
    fn on_error<'a>(&'a mut self, message: &'a str) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>>;

    /// A status update (e.g., "🤖 正在调用 ...", "🔧 正在执行工具: ...").
    fn on_status<'a>(&'a mut self, _message: &'a str) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        Box::pin(async {})
    }
}

/// Options for a single agent turn.
pub struct AgentTurnOptions<'a> {
    /// Mutable conversation history (system prompt at index 0).
    pub history: &'a mut Vec<ChatMessage>,
    /// Pre-filtered tool schemas (caller applies skill gating).
    pub tools: &'a [ToolDefinition],
    /// LLM provider to use.
    pub provider: Arc<dyn LlmProvider>,
    /// Tool registry for dispatching tool calls.
    pub tool_registry: Arc<ToolRegistry>,
    /// Tool execution context (user_id, workspace_root).
    pub tool_ctx: &'a ToolContext,
    /// Optional pre-context message injected after system prompt (e.g., reminders).
    pub pre_context: Option<&'a ChatMessage>,
    /// Maximum LLM turns before giving up.
    pub max_turns: usize,
    /// Timeout for the initial LLM call (seconds).
    pub llm_timeout_secs: u64,
    /// Timeout for each stream chunk (seconds).
    pub stream_timeout_secs: u64,
    /// Timeout for each tool call (seconds).
    pub tool_timeout_secs: u64,
    /// Output sink for streaming events to the client.
    pub output: &'a mut (dyn AgentOutputSink + Send),
    // ── Logging ──
    pub user_id: &'a str,
    pub model: &'a str,
    pub log_writer: Option<Arc<LogWriter>>,
}

/// Result of a single agent turn.
pub struct AgentTurnResult {
    /// The final text response from the LLM.
    pub response: String,
    /// Token usage from the final LLM call.
    pub usage: Option<(i32, i32, i32)>,
    /// Number of LLM turns used.
    pub turns_used: usize,
    /// Whether the turn completed normally (true) or hit max_turns (false).
    pub completed: bool,
}

/// Run a single agent turn: LLM call → tool dispatch → repeat until done.
///
/// This is the **production** agent loop implementation. It handles:
/// - LLM streaming with timeout protection
/// - Tool call parsing and dispatch with timeout
/// - Conversation history management
/// - Structured logging (LLM requests + tool calls)
/// - Pre-context injection (reminders, cron jobs)
///
/// The transport layer (WebSocket, Feishu, etc.) implements [`AgentOutputSink`]
/// to receive streaming events and forward them to the client.
pub async fn run_turn(opts: AgentTurnOptions<'_>) -> AgentTurnResult {
    let max_turns = opts.max_turns;
    let mut turns_used = 0;
    let mut final_response = String::new();
    let mut final_usage: Option<(i32, i32, i32)> = None;
    let mut completed = false;

    for turn in 0..max_turns {
        turns_used = turn + 1;

        // Build effective history: inject pre-context (reminders/cron) after system prompt
        let effective_history = match opts.pre_context {
            Some(ctx) if opts.history.len() >= 1 => {
                let mut h = opts.history.clone();
                h.insert(1, ctx.clone());
                h
            }
            _ => opts.history.clone(),
        };

        let llm_start = std::time::Instant::now();
        let provider_name = opts.provider.name().to_string();

        // Log LLM input
        let llm_input = build_llm_input(
            &effective_history
                .iter()
                .map(|m| (m.role.as_str(), m.content.as_str()))
                .collect::<Vec<_>>(),
        );

        // Send status for first turn or tool-follow-up
        if turn == 0 {
            opts.output
                .on_status(&format!("🤖 正在调用 {} ...", provider_name)).await;
        } else {
            opts.output.on_status(&format!(
                "🔄 工具调用完成，继续思考 (第{}轮)...",
                turn + 1
            )).await;
        }

        // Call LLM with timeout
        let stream_result = tokio::time::timeout(
            std::time::Duration::from_secs(opts.llm_timeout_secs),
            opts.provider.chat_stream(&effective_history, opts.tools),
        )
        .await;

        let mut stream = match stream_result {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => {
                opts.output
                    .on_error(&format!("LLM error: {}", e)).await;
                break;
            }
            Err(_) => {
                let elapsed = llm_start.elapsed().as_secs_f64();
                opts.output.on_error(&format!(
                    "LLM request timed out after {:.1}s. \
                     The attached document(s) may be too large for the model's context window. \
                     Try asking a more specific question or using a smaller document.",
                    elapsed
                )).await;
                break;
            }
        };

        // Process stream chunks
        let mut assistant_content = String::new();
        let mut reasoning_content = String::new();
        let mut current_tool_args: HashMap<String, (String, String, String)> = HashMap::new();
        let mut had_error = false;

        loop {
            let chunk = match tokio::time::timeout(
                std::time::Duration::from_secs(opts.stream_timeout_secs),
                stream.next(),
            )
            .await
            {
                Ok(Some(c)) => c,
                Ok(None) => break,
                Err(_) => {
                    opts.output
                        .on_error("LLM stream timed out. The response was too large or the connection was lost.").await;
                    had_error = true;
                    break;
                }
            };
            match chunk {
                Ok(StreamChunk::Delta(delta)) => {
                    assistant_content.push_str(&delta);
                    opts.output.on_text_delta(&delta).await;
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
                }
                Ok(StreamChunk::Done(usage)) => {
                    final_usage =
                        Some((usage.prompt_tokens, usage.completion_tokens, usage.total_tokens));
                }
                Err(e) => {
                    opts.output
                        .on_error(&format!("Stream error: {}", e)).await;
                    had_error = true;
                    break;
                }
            }
        }

        if had_error {
            if !assistant_content.is_empty() {
                final_response = assistant_content;
            }
            break;
        }

        // Build tool calls from accumulated deltas
        let tool_calls: Vec<ToolCall> = current_tool_args
            .into_iter()
            .map(|(_, (call_id, func_name, arguments))| ToolCall {
                id: call_id,
                r#type: "function".to_string(),
                function: FunctionCall {
                    name: func_name,
                    arguments,
                },
            })
            .collect();

        // Send tool_call_start events (with full arguments, after stream ends)
        for tc in &tool_calls {
            opts.output
                .on_tool_call_start(&tc.function.name, &tc.id, &tc.function.arguments).await;
        }

        // Log LLM request/response
        let llm_duration_ms = llm_start.elapsed().as_millis() as u64;
        let tool_names: Vec<String> =
            tool_calls.iter().map(|tc| tc.function.name.clone()).collect();
        if let Some(ref lw) = opts.log_writer {
            let log_entry = LogEntry::LlmRequest {
                timestamp: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                user_id: opts.user_id.to_string(),
                provider: provider_name,
                model: opts.model.to_string(),
                duration_ms: llm_duration_ms,
                input: llm_input,
                output: build_llm_output(
                    &assistant_content,
                    tool_calls.len(),
                    tool_names,
                    final_usage,
                ),
            };
            let lw = lw.clone();
            tokio::spawn(async move { lw.write(&log_entry).await });
        }

        // Add assistant message to history
        opts.history.push(ChatMessage {
            role: "assistant".to_string(),
            content: assistant_content.clone(),
            tool_calls: if tool_calls.is_empty() {
                None
            } else {
                Some(tool_calls.clone())
            },
            tool_call_id: None,
            reasoning_content: if reasoning_content.is_empty() {
                None
            } else {
                Some(reasoning_content)
            },
        });

        // If no tool calls, this is the final turn
        if tool_calls.is_empty() {
            final_response = assistant_content;
            opts.output.on_done(final_usage).await;
            completed = true;
            break;
        }

        // Dispatch tool calls
        opts.output.on_status(&format!(
            "🔧 正在执行工具: {}",
            tool_calls
                .iter()
                .map(|tc| tc.function.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )).await;

        let mut tool_results = Vec::new();
        for tc in &tool_calls {
            // Skip empty-name tool calls (ghost calls from some LLMs)
            if tc.function.name.trim().is_empty() {
                tracing::warn!(
                    call_id = %tc.id,
                    arguments = %tc.function.arguments,
                    "Skipping tool call with empty name"
                );
                opts.history.push(ChatMessage {
                    role: "tool".to_string(),
                    content: "Error: tool call had empty name, skipped".to_string(),
                    tool_calls: None,
                    tool_call_id: Some(tc.id.clone()),
                    reasoning_content: None,
                });
                continue;
            }

            // Skip empty-argument tool calls
            if tc.function.arguments.trim().is_empty() {
                tracing::debug!(
                    tool = %tc.function.name,
                    call_id = %tc.id,
                    "Skipping tool call with empty arguments"
                );
                opts.history.push(ChatMessage {
                    role: "tool".to_string(),
                    content: "Error: tool call had empty arguments, skipped".to_string(),
                    tool_calls: None,
                    tool_call_id: Some(tc.id.clone()),
                    reasoning_content: None,
                });
                continue;
            }

            let tool_start = std::time::Instant::now();
            let dispatch_result = tokio::time::timeout(
                std::time::Duration::from_secs(opts.tool_timeout_secs),
                opts.tool_registry.dispatch_isolated(
                    &tc.function.name,
                    &tc.function.arguments,
                    opts.tool_ctx,
                ),
            )
            .await;

            let result = match dispatch_result {
                Ok(r) => r,
                Err(_) => {
                    tracing::warn!(
                        tool = %tc.function.name,
                        "Tool call timed out after {}s",
                        opts.tool_timeout_secs
                    );
                    Err(anyhow::anyhow!("Tool call timed out"))
                }
            };

            let result_str = match result {
                Ok(r) => r,
                Err(e) => format!("Error: {}", e),
            };
            let tool_duration_ms = tool_start.elapsed().as_millis() as u64;

            opts.output
                .on_tool_call_end(&tc.function.name, &result_str).await;

            // Log tool call
            if let Some(ref lw) = opts.log_writer {
                let tool_log = ToolCallEntry::new(opts.user_id, &tc.function.name)
                    .into_log_entry_with(
                        &tc.function.arguments,
                        &result_str,
                        tool_duration_ms,
                    );
                let lw = lw.clone();
                tokio::spawn(async move { lw.write(&tool_log).await });
            }

            // Add tool result to history
            opts.history.push(ChatMessage {
                role: "tool".to_string(),
                content: result_str.clone(),
                tool_calls: None,
                tool_call_id: Some(tc.id.clone()),
                reasoning_content: None,
            });

            tool_results.push((
                tc.function.name.clone(),
                tc.function.arguments.clone(),
                result_str,
                tool_duration_ms,
            ));
        }
        // Loop continues — LLM will see tool results and respond
    }

    AgentTurnResult {
        response: final_response,
        usage: final_usage,
        turns_used,
        completed,
    }
}

// ─── Shared helper functions ──────────────────────────────────────────

/// Filter tool schemas by enabled skills.
///
/// Some tools are gated behind skills — they are only available when the
/// corresponding skill is enabled by the user. This function returns only
/// the tools that the user is allowed to use.
pub fn filter_tools_by_skill(
    all_schemas: &[ToolDefinition],
    enabled_skill_ids: &HashSet<String>,
) -> Vec<ToolDefinition> {
    all_schemas
        .iter()
        .filter(|t| {
            let skill_required = match t.function.name.as_str() {
                "web_search" => Some("builtin:web_search"),
                "excel_db" => Some("builtin:excel_data"),
                "file_read"
                | "file_write"
                | "file_list"
                | "file_delete"
                | "file_move"
                | "file_copy"
                | "file_search"
                | "dir_create"
                | "dir_list"
                | "file_info"
                | "shell" => Some("builtin:code_engineer"),
                _ => None,
            };
            match skill_required {
                Some(skill_id) => enabled_skill_ids.contains(skill_id),
                None => true,
            }
        })
        .cloned()
        .collect()
}

/// Inject document content into the system prompt (history[0]).
///
/// Called when the user attaches reference documents to a chat message.
/// The document content is appended to the system prompt with instructions
/// for the LLM to use it as reference.
pub fn inject_documents_to_system_prompt(
    history: &mut Vec<ChatMessage>,
    docs: &[(String, Option<String>, String)],
) {
    if docs.is_empty() || history.is_empty() {
        return;
    }

    let doc_count = docs.len();
    let doc_names: Vec<&str> = docs.iter().map(|(name, _, _)| name.as_str()).collect();

    let mut parts = Vec::new();
    for (name, path, content) in docs {
        if !content.is_empty() {
            let max_len = 15000;
            let content = if content.len() > max_len {
                let mut end = max_len;
                while end > 0 && !content.is_char_boundary(end) {
                    end -= 1;
                }
                format!(
                    "{}\n\n[... content truncated, {} chars total ...]",
                    &content[..end],
                    content.len()
                )
            } else {
                content.clone()
            };
            let path_info = path.as_deref().unwrap_or("");
            parts.push(format!(
                "=== {} (path: {}) ===\n{}\n=== END ===",
                name, path_info, content
            ));
        }
    }

    if !parts.is_empty() {
        let doc_block = format!(
            "\n\n## ATTACHED REFERENCE DOCUMENTS\n\
             The user has attached {} document(s) to this conversation: {}\n\
             Relevant sections have been retrieved using semantic search.\n\n\
             ⚠️ CRITICAL RULES — YOU MUST FOLLOW THESE:\n\
             1. Use the document content below to answer questions.\n\
             2. If the relevant sections don't contain the answer, say so.\n\
             3. You can use doc_retrieve tool for more specific searches.\n\
             4. Do NOT call pdf_parse, file_read to access these files - content is already provided.\n\n\
             {}\n",
            doc_count,
            doc_names.join(", "),
            parts.join("\n\n")
        );
        history[0].content = format!("{}{}", history[0].content, doc_block);
    }
}

/// Build Excel analysis guidance to append to the system prompt.
pub fn build_excel_analysis_guidance(
    user_content: &str,
    has_excel_docs: bool,
) -> Option<String> {
    let mentions_excel = user_content.contains("xlsx")
        || user_content.contains("xls")
        || user_content.contains("Excel")
        || user_content.contains("表格")
        || user_content.contains("工作表")
        || user_content.contains("query");

    if !mentions_excel && !has_excel_docs {
        return None;
    }

    Some(
        "\n\nExcel analysis guidance:\n\
         - If the user asks about an uploaded Excel file, prefer using excel_db first. \
         Start with action=\"list\" to find the imported database, then inspect schema if needed, \
         then run focused SELECT queries.\n\
         - Do not call file_list, file_search, or doc_list unless excel_db clearly shows the \
         workbook/database is missing or you need to locate the source file after excel_db fails.\n\
         - Avoid repeated exploratory tool loops. Once you have enough rows or aggregates to answer, \
         stop calling tools and provide a concise conclusion.\n\
         - For questions like '大家都在问什么' or query analysis, summarize recurring intents/themes, \
         representative examples, and notable frequencies/patterns from the data instead of only \
         dumping raw rows."
            .to_string(),
    )
}

/// Load the user's custom agent identity from memory.
pub async fn load_agent_identity(
    memory_manager: &MemoryManager,
    user_id: &str,
) -> Option<String> {
    match memory_manager.recall_all(user_id).await {
        Ok(entries) => entries
            .into_iter()
            .find(|e| e.category == "agent_identity")
            .map(|e| e.content),
        Err(_) => None,
    }
}

/// Build the skill prompt block from enabled skills.
///
/// Loads enabled skill IDs from memory, then fetches content from
/// builtin or user skills. Returns (prompt_string, set_of_enabled_skill_ids).
pub async fn build_skill_prompt(
    memory_manager: &MemoryManager,
    skill_manager: &SkillManager,
    user_id: &str,
) -> (String, HashSet<String>) {
    let enabled_ids: HashSet<String> = match memory_manager.recall_all(user_id).await {
        Ok(entries) => entries
            .into_iter()
            .filter(|e| e.category == "skill_enabled")
            .map(|e| e.content)
            .collect(),
        Err(_) => return (String::new(), HashSet::new()),
    };

    if enabled_ids.is_empty() {
        return (String::new(), enabled_ids);
    }

    let mut blocks: Vec<String> = Vec::new();

    for s in builtin_skills() {
        if enabled_ids.contains(s.id) {
            blocks.push(s.content.to_string());
        }
    }

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

    (
        format!(
            "\n\n---\n\n## Active Skills\n\n{}",
            blocks.join("\n\n---\n\n")
        ),
        enabled_ids,
    )
}

/// Build a system context message with active reminders and cron jobs.
/// Returns None if there are no active reminders or cron jobs.
pub fn build_reminder_context_msg(
    reminders: &[Reminder],
    cron_jobs: &[CronJob],
) -> Option<ChatMessage> {
    if reminders.is_empty() && cron_jobs.is_empty() {
        return None;
    }
    let mut parts = Vec::new();
    if !reminders.is_empty() {
        let lines: Vec<String> = reminders
            .iter()
            .map(|r| {
                format!("- [id: {}] \"{}\" (triggers at: {})", r.id, r.message, r.fire_at)
            })
            .collect();
        parts.push(format!("User's active reminders:\n{}", lines.join("\n")));
    }
    if !cron_jobs.is_empty() {
        let lines: Vec<String> = cron_jobs
            .iter()
            .map(|cj| format!("- [id: {}] \"{}\" (Schedule: {})", cj.id, cj.prompt, cj.schedule))
            .collect();
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

/// Build the system prompt with custom identity and skill prompt.
///
/// This is the canonical system prompt builder used by all transports.
/// It includes the agent identity, tool descriptions, current time, and
/// active skills.
pub fn build_system_prompt_with_identity(
    custom_identity: Option<&str>,
    skill_prompt: &str,
) -> String {
    let now = chrono::Local::now();
    let current_time = now.format("%Y年%m月%d日 %H时%M分%S秒 (%:z)").to_string();

    let identity = match custom_identity {
        Some(id) if !id.trim().is_empty() => id.to_string(),
        _ => "You are Jcowork Agent, an intelligent AI assistant.".to_string(),
    };

    format!(
r#"{identity}

**IMPORTANT: Current date and time: {current_time}**
When searching for latest news or events, ALWAYS use the current year ({current_year}) in your search queries. Do NOT use outdated years.

You have the following tools available:

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

**memory_update** — Update an existing memory entry with new or updated content.
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

Document search guidance:
- **If the user attached documents to this conversation, the content is already provided above.** Read it directly — do NOT call doc_retrieve for attached documents.
- Use **doc_retrieve** for all document searches. It automatically tries semantic search first, then falls back to keyword search. One tool call handles everything.
- Keep the doc_retrieve `query` argument short and copied from the user's original words (e.g. 用户问"雨的四季全文" → query 就是"雨的四季"). NEVER add author names, synonyms, or filler words like 全文/内容/课文 — extra words dilute the embedding and hurt recall.
- When fragments are not enough (e.g. the user asks for the full text 完整全文 or broader context): each doc_retrieve result carries its Offset in the document — call doc_content with that file_path and offset to read forward from the fragment's position. Read only until you have enough to answer, then stop; do NOT read the whole document unnecessarily, and do NOT piece together an answer from scattered fragments alone.
- If doc_retrieve returns no results, use doc_list to see what documents are available, then inform the user.
- Avoid repeated tool loops. After 1-2 search attempts, provide your best answer or tell the user what you found.

File path rules:
- All file tools (file_read, file_write, file_list, etc.) use paths RELATIVE to the workspace root. NEVER use absolute paths like /Users/xxx/... — they will be rejected.
- When the user mentions a file they uploaded, use just the filename (e.g. 江家现金流.xlsx) or the relative path shown by file_list.
- For Excel files, prefer excel_db tool over file_read. Excel files are binary and file_read cannot display their data.

今天是 {current_time}。当前年份是 {current_year}，搜索最新消息时务必使用 {current_year} 年而非其他年份。

IMPORTANT: When the user asks to set a reminder or alarm, DO NOT give instructions on how to use their phone's clock app. Instead, USE the reminder_add tool to actually set the reminder in the system. NEVER write out a list of reminders in text without calling the tool — that is a hallucination, not a real reminder.{skill_prompt}"#,
        identity = identity,
        current_time = current_time,
        current_year = now.format("%Y").to_string(),
        skill_prompt = skill_prompt
    )
}
