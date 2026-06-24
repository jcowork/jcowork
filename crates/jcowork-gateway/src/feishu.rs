//! Feishu event callback handler.
//!
//! Receives events from Feishu, runs the agent loop, and replies via Feishu API.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use axum::response::IntoResponse;
use futures::StreamExt;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use jcowork_feishu::{crypto, event};
use jcowork_llm::provider::{ChatMessage, StreamChunk, ToolCall};
use jcowork_logs::{build_llm_input, build_llm_output, LogEntry, ToolCallEntry};
use jcowork_memory::MemoryManager;
use jcowork_skills::{builtin_skills, SkillManager};
use jcowork_tools::base::ToolContext;

use crate::ws;

/// Handle POST /api/feishu/event
pub async fn feishu_event_handler(
    State(state): State<crate::router::AppState>,
    _headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    // Parse the incoming event
    let fe_event: event::FeishuEvent = match serde_json::from_value(body) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(err = %e, "Failed to parse Feishu event");
            return (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": "invalid event"})));
        }
    };

    // Extract app_id from event header to route to the correct user
    let event_app_id = fe_event.header.as_ref()
        .and_then(|h| h.app_id.clone())
        .or_else(|| fe_event.token.clone()); // fallback: some events use token field

    // Handle challenge/verification request
    if fe_event.is_challenge() {
        let challenge = fe_event.challenge.as_deref().unwrap_or("");
        // Feishu challenge request contains verification_token in the "token" field
        // First try to use the token from the request, then fall back to looking up by app_id
        let verification_token = if let Some(token) = fe_event.token.clone() {
            token
        } else if let Some(ref app_id) = event_app_id {
            // Fallback: look up verification token from the config store by app_id
            state.feishu_config_store.get_by_app_id(app_id)
                .await
                .ok()
                .flatten()
                .map(|c| c.verification_token)
                .unwrap_or_default()
        } else {
            String::new()
        };
        let resp = crypto::challenge_response(challenge, &verification_token);
        return (StatusCode::OK, Json(resp));
    }

    // We need an app_id to route the event
    let app_id = match event_app_id {
        Some(id) => id,
        None => {
            tracing::warn!("Feishu event missing app_id, cannot route");
            return (StatusCode::OK, Json(serde_json::json!({"status": "ignored"})));
        }
    };

    // Parse the message from the event
    let msg = match fe_event.parse_message() {
        Some(m) => m,
        None => {
            return (StatusCode::OK, Json(serde_json::json!({"status": "ignored"})));
        }
    };

    tracing::info!(app_id = %app_id, open_id = %msg.open_id, text = %msg.text, "Feishu message received");

    // Spawn the agent handler in the background so we can return 200 immediately
    let state_clone = state.clone();
    tokio::spawn(async move {
        if let Err(e) = handle_feishu_message(&state_clone, &app_id, &msg).await {
            tracing::error!(err = %e, "Feishu agent handler error");
        }
    });

    (StatusCode::OK, Json(serde_json::json!({"status": "ok"})))
}

/// Process a Feishu message through the agent loop and reply.
async fn handle_feishu_message(
    state: &crate::router::AppState,
    app_id: &str,
    msg: &event::ParsedMessage,
) -> anyhow::Result<()> {
    // Look up the jcowork user who owns this Feishu app
    let config = state.feishu_config_store.get_by_app_id(app_id).await?
        .ok_or_else(|| anyhow::anyhow!("No jcowork user found for Feishu app_id: {}", app_id))?;
    let user_id = config.user_id.clone();

    // Get or create a FeishuClient for this app_id (cached)
    let feishu_client = state.feishu_client_cache
        .entry(app_id.to_string())
        .or_insert_with(|| {
            Arc::new(jcowork_feishu::client::FeishuClient::new(
                config.app_id.clone(),
                config.app_secret.clone(),
            ))
        })
        .clone();

    // Load user's custom agent identity from memory
    let custom_identity = load_agent_identity(&state.memory_manager, &user_id).await;

    // Load enabled skills and build skill prompt blocks
    let (skill_prompt, enabled_skill_ids) = build_skill_prompt(
        &state.memory_manager, &state.skill_manager, &user_id,
    ).await;

    // Build system prompt
    let system_prompt = ws::build_system_prompt_with_identity(custom_identity.as_deref(), &skill_prompt);
    let mut history: Vec<ChatMessage> = vec![ChatMessage {
        role: "system".to_string(),
        content: system_prompt,
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
    }];

    // Add user message
    history.push(ChatMessage {
        role: "user".to_string(),
        content: msg.text.clone(),
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
    });

    // Resolve model
    let model_str = &state.default_model;
    let provider = state.llm_router.get_provider(model_str)?;

    // Get tool schemas — filter skill-gated tools
    let tools: Vec<_> = state.tool_registry.all_schemas()
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

    // Fetch active reminders/cron jobs for context
    let active_reminders = state.cron_scheduler.list_reminders(&user_id).await;
    let active_cron_jobs = state.cron_scheduler.list_cron_jobs(&user_id).await;
    let reminder_ctx_msg = build_reminder_context_msg(&active_reminders, &active_cron_jobs);

    // Agent loop (non-streaming — collect full text, then reply)
    let max_turns = 10;
    let mut final_response = String::new();

    for _turn in 0..max_turns {
        // Build effective history with reminder context
        let effective_history = match &reminder_ctx_msg {
            Some(ctx) if history.len() >= 1 => {
                let mut h = history.clone();
                h.insert(1, ctx.clone());
                h
            }
            _ => history.clone(),
        };

        let llm_start = std::time::Instant::now();
        let llm_input = build_llm_input(&effective_history.iter().map(|m| (m.role.as_str(), m.content.as_str())).collect::<Vec<_>>());
        let provider_name = provider.name().to_string();

        let stream_result = provider.chat_stream(&effective_history, &tools).await;
        let mut stream = match stream_result {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(err = %e, "Feishu LLM stream error");
                final_response = format!("Sorry, an error occurred: {}", e);
                break;
            }
        };

        let mut assistant_content = String::new();
        let mut reasoning_content = String::new();
        let mut current_tool_args: HashMap<String, (String, String, String)> = HashMap::new();
        let mut had_error = false;
        let mut final_usage: Option<(i32, i32, i32)> = None;

        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(StreamChunk::Delta(delta)) => {
                    assistant_content.push_str(&delta);
                }
                Ok(StreamChunk::ReasoningDelta(reasoning)) => {
                    reasoning_content.push_str(&reasoning);
                }
                Ok(StreamChunk::ToolCallDelta(call_id, name, args_delta)) => {
                    let entry = current_tool_args
                        .entry(call_id.clone())
                        .or_insert_with(|| (call_id.clone(), name.clone(), String::new()));
                    entry.2.push_str(&args_delta);
                }
                Ok(StreamChunk::Done(usage)) => {
                    final_usage = Some((usage.prompt_tokens, usage.completion_tokens, usage.total_tokens));
                }
                Err(e) => {
                    tracing::error!(err = %e, "Feishu stream chunk error");
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
            output: build_llm_output(&assistant_content, tool_calls.len(), tool_names, final_usage),
        };
        let lw = state.log_writer.clone();
        tokio::spawn(async move { lw.write(&log_entry).await });

        // Add assistant message to history
        history.push(ChatMessage {
            role: "assistant".to_string(),
            content: assistant_content,
            tool_calls: if tool_calls.is_empty() { None } else { Some(tool_calls.clone()) },
            tool_call_id: None,
            reasoning_content: if reasoning_content.is_empty() { None } else { Some(reasoning_content) },
        });

        // If no tool calls, this is the final turn
        if tool_calls.is_empty() {
            // The final response is the assistant content
            if let Some(last) = history.last() {
                if last.role == "assistant" {
                    final_response = last.content.clone();
                }
            }
            break;
        }

        // Dispatch tool calls (skip empty-argument ghost calls)
        for tc in &tool_calls {
            if tc.function.arguments.trim().is_empty() {
                tracing::debug!(tool = %tc.function.name, call_id = %tc.id, "Skipping empty-arg tool call");
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
            let result = state.tool_registry
                .dispatch(&tc.function.name, &tc.function.arguments, &tool_ctx)
                .await;

            let result_str = match result {
                Ok(r) => r,
                Err(e) => format!("Error: {}", e),
            };

            let tool_duration_ms = tool_start.elapsed().as_millis() as u64;

            // Log tool call
            let tool_log = ToolCallEntry::new(&user_id, &tc.function.name)
                .into_log_entry_with(&tc.function.arguments, &result_str, tool_duration_ms);
            let lw = state.log_writer.clone();
            tokio::spawn(async move { lw.write(&tool_log).await });

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

    // Reply to the Feishu message
    if !final_response.is_empty() {
        feishu_client.reply_message(&msg.message_id, &final_response).await?;
    } else {
        feishu_client.reply_message(&msg.message_id, "抱歉，我暂时无法回复。").await?;
    }

    Ok(())
}

/// Load the user's custom agent identity from memory.
async fn load_agent_identity(memory_manager: &MemoryManager, user_id: &str) -> Option<String> {
    memory_manager
        .recall_all(user_id)
        .await
        .ok()?
        .into_iter()
        .find(|e| e.category == "agent_identity")
        .map(|e| e.content)
}

/// Build the skill prompt block from enabled skills.
async fn build_skill_prompt(
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
        format!("\n\n---\n\n## Active Skills\n\n{}", blocks.join("\n\n---\n\n")),
        enabled_ids,
    )
}

/// Build a system context message containing the user's active reminders and cron jobs.
fn build_reminder_context_msg(
    reminders: &[jcowork_cron::Reminder],
    cron_jobs: &[jcowork_cron::CronJob],
) -> Option<ChatMessage> {
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
