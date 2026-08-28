//! Feishu event callback handler.
//!
//! Receives events from Feishu, runs the agent loop, and replies via Feishu API.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use axum::response::IntoResponse;
use std::sync::Arc;

use jcowork_agent::r#loop as agent_loop;
use jcowork_agent::r#loop::{AgentOutputSink, AgentTurnOptions, AgentTurnResult};
use jcowork_feishu::{crypto, event};
use jcowork_llm::provider::ChatMessage;
use jcowork_tools::base::ToolContext;

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
    let custom_identity = agent_loop::load_agent_identity(&state.memory_manager, &user_id).await;

    // Load enabled skills and build skill prompt blocks
    let (skill_prompt, enabled_skill_ids) = agent_loop::build_skill_prompt(
        &state.memory_manager, &state.skill_manager, &user_id,
    ).await;

    // Build system prompt
    let system_prompt =
        agent_loop::build_system_prompt_with_identity(custom_identity.as_deref(), &skill_prompt);
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
    let provider = state.llm_router.read().unwrap().get_provider(model_str)?;

    // Get tool schemas — filter skill-gated tools
    let tools = agent_loop::filter_tools_by_skill(
        &state.tool_registry.all_schemas(), &enabled_skill_ids,
    );

    // Compute per-user workspace root and ensure it exists
    let workspace_root = format!("{}/{}/workspace", state.data_dir, user_id);
    let _ = tokio::fs::create_dir_all(&workspace_root).await;
    let tool_ctx = ToolContext {
        user_id: user_id.clone(),
        workspace_root,
    };

    // Fetch active reminders/cron jobs for context
    let active_reminders = state.cron_scheduler.list_reminders(&user_id).await;
    let active_cron_jobs = state.cron_scheduler.list_cron_jobs(&user_id).await;
    let reminder_ctx_msg = agent_loop::build_reminder_context_msg(&active_reminders, &active_cron_jobs);

    // ── Run agent turn via shared run_turn() ──
    let mut sink = FeishuSink::new();
    let result: AgentTurnResult = agent_loop::run_turn(AgentTurnOptions {
        history: &mut history,
        tools: &tools,
        provider,
        tool_registry: state.tool_registry.clone(),
        tool_ctx: &tool_ctx,
        pre_context: reminder_ctx_msg.as_ref(),
        max_turns: 10,
        llm_timeout_secs: 60,
        stream_timeout_secs: 120,
        tool_timeout_secs: 30,
        output: &mut sink,
        user_id: &user_id,
        model: model_str,
        log_writer: Some(state.log_writer.clone()),
    }).await;

    // Reply to the Feishu message
    let response = if !result.response.is_empty() {
        &result.response
    } else {
        "抱歉，我暂时无法回复。"
    };
    feishu_client.reply_message(&msg.message_id, response).await?;

    Ok(())
}

// ─── Feishu output sink ─────────────────────────────────────────────

/// Feishu output sink — collects text deltas from the agent loop.
/// Unlike WebSocket, Feishu doesn't stream — we send the final reply
/// after the agent turn completes.
struct FeishuSink {
    response: String,
}

impl FeishuSink {
    fn new() -> Self {
        Self { response: String::new() }
    }
}

impl AgentOutputSink for FeishuSink {
    fn on_text_delta<'a>(&'a mut self, text: &'a str) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        self.response.push_str(text);
        Box::pin(async {})
    }

    fn on_tool_call_start<'a>(&'a mut self, _name: &'a str, _call_id: &'a str, _arguments: &'a str) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        Box::pin(async {})
    }

    fn on_tool_call_end<'a>(&'a mut self, _name: &'a str, _result: &'a str) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        Box::pin(async {})
    }

    fn on_done<'a>(&'a mut self, _usage: Option<(i32, i32, i32)>) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        Box::pin(async {})
    }

    fn on_error<'a>(&'a mut self, message: &'a str) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        self.response = format!("Sorry, an error occurred: {}", message);
        Box::pin(async {})
    }

    fn on_status<'a>(&'a mut self, _message: &'a str) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        Box::pin(async {})
    }
}
