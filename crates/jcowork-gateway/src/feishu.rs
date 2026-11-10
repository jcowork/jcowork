//! Feishu event handling shared by the webhook and the long-connection bot.
//!
//! [`feishu_turn`] runs one agent turn on behalf of a Feishu message; the
//! webhook handler below and the persistent bot in [`crate::feishu_bot`] both
//! call it, so a message answered over the callback URL and one answered over
//! the long connection behave identically (same skills, same tools, same
//! model choice).

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use std::sync::Arc;

use jcowork_agent::r#loop as agent_loop;
use jcowork_agent::r#loop::{AgentOutputSink, AgentTurnOptions, AgentTurnResult};
use jcowork_feishu::client::FeishuClient;
use jcowork_feishu::{crypto, event};
use jcowork_llm::provider::ChatMessage;
use jcowork_tools::base::ToolContext;

/// Prior turns kept per Feishu chat before older context is dropped.
pub(crate) const MAX_CHAT_HISTORY: usize = 20;

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

    let client = client_for(state, &config.app_id, &config.app_secret);
    let mut history: Vec<ChatMessage> = Vec::new();
    let response = feishu_turn(state, &config.user_id, &msg.text, &mut history).await?;
    client.reply_message(&msg.message_id, &response).await?;

    Ok(())
}

/// The cached [`FeishuClient`] for an app (one tenant-token cache per app).
pub(crate) fn client_for(
    state: &crate::router::AppState,
    app_id: &str,
    app_secret: &str,
) -> Arc<FeishuClient> {
    state
        .feishu_client_cache
        .entry(app_id.to_string())
        .or_insert_with(|| {
            Arc::new(FeishuClient::new(app_id.to_string(), app_secret.to_string()))
        })
        .clone()
}

/// Run one agent turn for a Feishu message and return the reply text.
///
/// `history` holds the chat's previous user/assistant turns (no system
/// prompt); the call appends this exchange and trims the oldest entries, so a
/// Feishu conversation keeps its context across messages.
///
/// The model is the one the user picked in Settings (persisted server-side by
/// `PUT /api/settings/model`), falling back to the deployment default.
pub(crate) async fn feishu_turn(
    state: &crate::router::AppState,
    user_id: &str,
    text: &str,
    history: &mut Vec<ChatMessage>,
) -> anyhow::Result<String> {
    // Ignore messages when the owning account no longer exists or is in the
    // recycle bin — trashed users must not run tasks.
    match state.user_store.get_user_by_id(user_id).await {
        Ok(Some(user)) if user.deleted_at.is_none() => {}
        Ok(Some(_)) => anyhow::bail!("Feishu owner is in the trash"),
        Ok(None) => anyhow::bail!("Feishu owner account not found"),
        Err(e) => anyhow::bail!("Failed to verify Feishu owner: {}", e),
    }

    let preferred = state
        .user_settings
        .get_model(user_id)
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| state.default_model.clone());
    // A saved model can become unavailable (provider removed, key revoked).
    // The bot must still answer, so fall back to the deployment default.
    let (model_str, provider) = {
        let router = state.llm_router.read().unwrap();
        match router.get_provider(&preferred) {
            Ok(provider) => (preferred, provider),
            Err(e) if preferred != state.default_model => {
                tracing::warn!(model = %preferred, error = %e, "Saved model unavailable; Feishu turn falls back to the default model");
                (
                    state.default_model.clone(),
                    router.get_provider(&state.default_model)?,
                )
            }
            Err(e) => return Err(e),
        }
    };

    // Load the user's custom agent identity and enabled skills.
    let custom_identity = agent_loop::load_agent_identity(&state.memory_manager, user_id).await;
    let (skill_prompt, enabled_skill_ids) =
        agent_loop::build_skill_prompt(&state.memory_manager, &state.skill_manager, user_id).await;
    let system_prompt =
        agent_loop::build_system_prompt_with_identity(custom_identity.as_deref(), &skill_prompt);

    let mut turn_history: Vec<ChatMessage> = Vec::with_capacity(history.len() + 3);
    turn_history.push(system_message(system_prompt));
    turn_history.extend(history.iter().cloned());
    turn_history.push(user_message(text.to_string()));

    // Compute per-user workspace root and ensure it exists.
    let workspace_root = format!("{}/{}/workspace", state.data_dir, user_id);
    let _ = tokio::fs::create_dir_all(&workspace_root).await;
    let tool_ctx = ToolContext {
        user_id: user_id.to_string(),
        workspace_root,
        mentioned_public_users: Vec::new(),
        model: Some(model_str.clone()),
    };

    let tools = agent_loop::filter_tools_by_skill(
        &state.tool_registry.all_schemas(),
        &enabled_skill_ids,
    );

    // Fetch active reminders/cron jobs for context.
    let active_reminders = state.cron_scheduler.list_reminders(user_id).await;
    let active_cron_jobs = state.cron_scheduler.list_cron_jobs(user_id).await;
    let reminder_ctx_msg =
        agent_loop::build_reminder_context_msg(&active_reminders, &active_cron_jobs);

    let mut sink = FeishuSink::new();
    let result: AgentTurnResult = agent_loop::run_turn(AgentTurnOptions {
        history: &mut turn_history,
        tools: &tools,
        provider,
        tool_registry: state.tool_registry.clone(),
        tool_ctx: &tool_ctx,
        pre_context: reminder_ctx_msg.as_ref(),
        // Match the chat budget (ws.rs). The final round is reserved for a
        // tools-off summary by run_turn.
        max_turns: 30,
        llm_timeout_secs: 60,
        stream_timeout_secs: 120,
        tool_timeout_secs: 300,
        output: &mut sink,
        user_id,
        model: &model_str,
        log_writer: Some(state.log_writer.clone()),
    })
    .await;

    let response = if !result.response.is_empty() {
        result.response
    } else if !sink.response.is_empty() {
        sink.response
    } else {
        "抱歉，我暂时无法回复。".to_string()
    };

    // Keep conversational context for the next Feishu message.
    history.push(user_message(text.to_string()));
    history.push(ChatMessage {
        role: "assistant".to_string(),
        content: response.clone(),
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
        images: None,
    });
    if history.len() > MAX_CHAT_HISTORY {
        let excess = history.len() - MAX_CHAT_HISTORY;
        history.drain(0..excess);
    }

    Ok(response)
}

fn system_message(content: String) -> ChatMessage {
    ChatMessage {
        role: "system".to_string(),
        content,
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
        images: None,
    }
}

fn user_message(content: String) -> ChatMessage {
    ChatMessage {
        role: "user".to_string(),
        content,
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
        images: None,
    }
}

// ─── Feishu output sink ─────────────────────────────────────────────

/// Feishu output sink — collects text deltas from the agent loop.
/// Unlike WebSocket, Feishu doesn't stream — we send the final reply
/// after the agent turn completes.
pub(crate) struct FeishuSink {
    pub(crate) response: String,
}

impl FeishuSink {
    pub(crate) fn new() -> Self {
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
        self.response = format!("抱歉，处理消息时出错：{}", message);
        Box::pin(async {})
    }

    fn on_status<'a>(&'a mut self, _message: &'a str) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        Box::pin(async {})
    }
}
