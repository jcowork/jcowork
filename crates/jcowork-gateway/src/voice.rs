//! Full-duplex voice call channel.
//!
//! Browser mic PCM (16 kHz s16le mono, binary WS frames) is relayed to the
//! Qwen Omni realtime ASR session; server VAD segments utterances and the
//! final transcript runs through the regular agent loop (same RunningTask
//! machinery as the text chat, so "stop" and task occupancy interoperate).
//! Reply text deltas are forwarded to the client AND sentence-segmented into
//! the Qwen TTS realtime session, whose PCM audio (24 kHz) streams back as
//! base64 `tts_audio` events.
//!
//! Barge-in: `input_audio_buffer.speech_started` aborts the running agent
//! task, drops the TTS connection and tells the client to flush playback.

use axum::extract::ws::{Message, WebSocket};
use base64::Engine;
use futures::{SinkExt, StreamExt};
use std::time::Instant;
use tokio::sync::mpsc;

use jcowork_agent::r#loop as agent_loop;
use jcowork_agent::r#loop::AgentTurnOptions;
use jcowork_logs::build_voice_stage;
use jcowork_tools::base::ToolContext;
use jcowork_voice::{AsrEvent, OmniAsrClient, SpeechSegmenter, TtsClient, TtsEvent, VoiceConfig};

use crate::router::AppState;
use crate::session::RunningTask;
use crate::ws::TaskSink;

type WsSender = futures::stream::SplitSink<WebSocket, Message>;

async fn send_ev(sender: &mut WsSender, v: serde_json::Value) {
    let _ = sender.send(Message::Text(v.to_string().into())).await;
}

/// Fire-and-forget a voice pipeline milestone into the daily JSONL log so
/// end-to-end latency (ASR → agent → TTS) can be attributed after the fact.
fn log_stage(
    state: &AppState,
    user_id: &str,
    conv: &str,
    epoch: Instant,
    stage: &str,
    data: serde_json::Value,
) {
    let writer = state.log_writer.clone();
    let entry = build_voice_stage(user_id, conv, stage, epoch.elapsed().as_millis() as u64, data);
    tokio::spawn(async move {
        writer.write(&entry).await;
    });
}

/// Per-call TTS measurement state for the stage log.
#[derive(Default)]
struct TtsTrace {
    connect_at: Option<Instant>,
    first_audio_logged: bool,
    bytes: u64,
}

/// Lazily (re)connect the per-reply TTS session.
#[allow(clippy::too_many_arguments)]
async fn ensure_tts(
    cfg: &VoiceConfig,
    tts: &mut Option<TtsClient>,
    tts_rx: &mut Option<mpsc::Receiver<TtsEvent>>,
    sender: &mut WsSender,
    state: &AppState,
    user_id: &str,
    conv: &str,
    epoch: Instant,
    trace: &mut TtsTrace,
) {
    if tts.is_some() {
        return;
    }
    let connect_at = Instant::now();
    match TtsClient::connect(cfg).await {
        Ok((client, rx)) => {
            *tts = Some(client);
            *tts_rx = Some(rx);
            *trace = TtsTrace { connect_at: Some(Instant::now()), ..Default::default() };
            log_stage(state, user_id, conv, epoch, "tts_connect", serde_json::json!({
                "connect_ms": connect_at.elapsed().as_millis() as u64,
            }));
        }
        Err(e) => {
            log_stage(state, user_id, conv, epoch, "tts_error", serde_json::json!({
                "source": "connect", "message": e.to_string(),
            }));
            send_ev(sender, serde_json::json!({
                "type": "error", "message": format!("语音合成连接失败: {}", e),
            }))
            .await;
        }
    }
}

/// Handle one voice call WebSocket.
pub async fn voice_ws_handler(ws: WebSocket, user_id: String, conv: String, state: AppState) {
    let (mut ws_sender, mut ws_receiver) = ws.split();
    // Snapshot the live config: an admin edit mid-call applies to the next
    // call, not to the speech sessions already open on this one.
    let voice_cfg = state.voice.read().expect("voice config lock poisoned").clone();

    if !voice_cfg.is_configured() {
        send_ev(&mut ws_sender, serde_json::json!({
            "type": "error",
            "message": "语音服务未配置：请在服务端设置 DASHSCOPE_API_KEY 和 JCWORK_VOICE_WORKSPACE_ID",
        }))
        .await;
        return;
    }

    // ── Wait for the start message (history + model) ──
    let mut history: Vec<jcowork_llm::provider::ChatMessage> = Vec::new();
    let mut model = state.default_model.clone();
    loop {
        match ws_receiver.next().await {
            Some(Ok(Message::Text(text))) => {
                let v: serde_json::Value = match serde_json::from_str(&text) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                match v["type"].as_str().unwrap_or("") {
                    "start" => {
                        if let Some(m) = v["model"].as_str() {
                            if !m.is_empty() {
                                model = m.to_string();
                            }
                        }
                        if let Some(msgs) = v["history"].as_array() {
                            for m in msgs {
                                let role = m["role"].as_str().unwrap_or("");
                                let content = m["content"].as_str().unwrap_or("");
                                if (role == "user" || role == "assistant") && !content.trim().is_empty() {
                                    history.push(jcowork_llm::provider::ChatMessage {
                                        role: role.to_string(),
                                        content: content.to_string(),
                                        tool_calls: None,
                                        tool_call_id: None,
                                        reasoning_content: None,
                                        images: None,
                                    });
                                }
                            }
                        }
                        break;
                    }
                    "stop" => return,
                    _ => continue,
                }
            }
            Some(Ok(Message::Close(_))) | None => return,
            Some(Err(_)) => return,
            _ => continue,
        }
    }

    // ── Identity, skills and system prompt (same as the text chat) ──
    let call_epoch = Instant::now();
    log_stage(&state, &user_id, &conv, call_epoch, "session_start", serde_json::json!({
        "model": model, "history_len": history.len(),
    }));
    let mut turn_epoch: Option<Instant> = None;
    let mut turn_seq: u32 = 0;
    let mut first_text_logged = false;
    let mut tts_trace = TtsTrace::default();
    let custom_identity = agent_loop::load_agent_identity(&state.memory_manager, &user_id).await;
    let (skill_prompt, enabled_skill_ids) =
        agent_loop::build_skill_prompt(&state.memory_manager, &state.skill_manager, &user_id).await;
    let system_prompt =
        agent_loop::build_system_prompt_with_identity(custom_identity.as_deref(), &skill_prompt);
    history.insert(0, jcowork_llm::provider::ChatMessage {
        role: "system".to_string(),
        content: system_prompt.clone(),
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
        images: None,
    });

    // ── Upstream ASR session ──
    let (asr, mut asr_rx) = match OmniAsrClient::connect(&voice_cfg).await {
        Ok(v) => v,
        Err(e) => {
            log_stage(&state, &user_id, &conv, call_epoch, "asr_error", serde_json::json!({
                "source": "connect", "message": e.to_string(),
            }));
            send_ev(&mut ws_sender, serde_json::json!({
                "type": "error", "message": format!("语音识别连接失败: {}", e),
            }))
            .await;
            return;
        }
    };

    let mut live_task_rx: Option<tokio::sync::broadcast::Receiver<String>> = None;
    let mut tts: Option<TtsClient> = None;
    let mut tts_rx: Option<mpsc::Receiver<TtsEvent>> = None;
    let mut segmenter = SpeechSegmenter::new();

    loop {
        tokio::select! {
            // ── Client frames: binary mic audio / JSON control ──
            msg = ws_receiver.next() => {
                match msg {
                    Some(Ok(Message::Binary(buf))) => asr.append_audio(&buf),
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                            if v["type"].as_str() == Some("stop") {
                                break;
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Err(_)) => break,
                    _ => {}
                }
            }
            // ── ASR events ──
            ev = asr_rx.recv() => {
                match ev {
                    Some(AsrEvent::Ready) => {
                        log_stage(&state, &user_id, &conv, call_epoch, "asr_ready", serde_json::json!({}));
                        send_ev(&mut ws_sender, serde_json::json!({"type": "voice_ready"})).await;
                    }
                    Some(AsrEvent::SpeechStarted) => {
                        // Barge-in: abort any running reply and flush audio.
                        let mut aborted = false;
                        if let Some(task) = state.session_manager.get_task(&conv) {
                            if task.abort() {
                                aborted = true;
                                task.emit(serde_json::json!({"type": "stopped"}).to_string());
                            }
                        }
                        log_stage(&state, &user_id, &conv, call_epoch, "barge_in", serde_json::json!({
                            "aborted_task": aborted,
                        }));
                        tts = None;
                        tts_rx = None;
                        segmenter = SpeechSegmenter::new();
                        send_ev(&mut ws_sender, serde_json::json!({"type": "tts_cancel"})).await;
                        send_ev(&mut ws_sender, serde_json::json!({"type": "user_speaking"})).await;
                    }
                    Some(AsrEvent::TranscriptDelta { text, stash }) => {
                        send_ev(&mut ws_sender, serde_json::json!({
                            "type": "asr_partial", "text": format!("{}{}", text, stash),
                        })).await;
                    }
                    Some(AsrEvent::TranscriptCompleted { transcript }) => {
                        let transcript = transcript.trim().to_string();
                        if transcript.is_empty() {
                            continue;
                        }
                        turn_seq += 1;
                        turn_epoch = Some(Instant::now());
                        first_text_logged = false;
                        log_stage(&state, &user_id, &conv, call_epoch, "asr_final", serde_json::json!({
                            "seq": turn_seq, "text": transcript,
                        }));
                        send_ev(&mut ws_sender, serde_json::json!({
                            "type": "asr_final", "text": transcript,
                        })).await;

                        // A text-chat task may still be running for this
                        // conversation — voice input interrupts it.
                        if let Some(task) = state.session_manager.get_task(&conv) {
                            if task.is_running() {
                                task.abort();
                                task.emit(serde_json::json!({"type": "stopped"}).to_string());
                            } else if let Some(h) = task.take_final_history() {
                                history = h;
                            }
                        }

                        // Reset the system prompt and append the utterance.
                        if !history.is_empty() {
                            history[0].content = system_prompt.clone();
                        }
                        history.push(jcowork_llm::provider::ChatMessage {
                            role: "user".to_string(),
                            content: transcript.clone(),
                            tool_calls: None,
                            tool_call_id: None,
                            reasoning_content: None,
                            images: None,
                        });

                        let provider = {
                            let router = state.llm_router.read().unwrap();
                            router.get_provider(&model)
                        };
                        let provider = match provider {
                            Ok(p) => p,
                            Err(e) => {
                                history.pop();
                                send_ev(&mut ws_sender, serde_json::json!({
                                    "type": "error", "message": e.to_string(),
                                })).await;
                                continue;
                            }
                        };

                        let mut tools = agent_loop::filter_tools_by_skill(
                            &state.tool_registry.all_schemas(), &enabled_skill_ids,
                        );
                        // Public-account tools need @mentions, which voice
                        // input does not carry.
                        tools.retain(|t| {
                            t.function.name != "public_doc_search"
                                && t.function.name != "public_doc_content"
                        });

                        let workspace_root = format!("{}/{}/workspace", state.data_dir, user_id);
                        let _ = tokio::fs::create_dir_all(&workspace_root).await;
                        let tool_ctx = ToolContext {
                            user_id: user_id.clone(),
                            workspace_root,
                            source_root: None,
                            mentioned_public_users: Vec::new(),
                            model: Some(model.clone()),
                        };
                        let active_reminders = state.cron_scheduler.list_reminders(&user_id).await;
                        let active_cron_jobs = state.cron_scheduler.list_cron_jobs(&user_id).await;
                        let pre_context = agent_loop::build_reminder_context_msg(
                            &active_reminders, &active_cron_jobs,
                        );

                        // Run the turn as a detached background task keyed by
                        // the conversation — shared with the text chat.
                        let task = RunningTask::new(transcript.clone());
                        state.session_manager.set_task(&conv, task.clone());
                        let (task_rx, _snapshot) = task.attach();
                        live_task_rx = Some(task_rx);
                        segmenter = SpeechSegmenter::new();

                        let history_snapshot = history.clone();
                        let user_id_task = user_id.clone();
                        let model_task = model.clone();
                        let task_for_spawn = task.clone();
                        let tool_registry_task = state.tool_registry.clone();
                        let log_writer_task = state.log_writer.clone();

                        let join = tokio::spawn(async move {
                            struct RunGuard(std::sync::Arc<RunningTask>);
                            impl Drop for RunGuard {
                                fn drop(&mut self) {
                                    self.0.set_finished();
                                }
                            }
                            let _guard = RunGuard(task_for_spawn.clone());

                            let mut sink = TaskSink { task: task_for_spawn.clone() };
                            let mut task_history = history_snapshot;

                            let result = agent_loop::run_turn(AgentTurnOptions {
                                history: &mut task_history,
                                tools: &tools,
                                provider,
                                tool_registry: tool_registry_task,
                                tool_ctx: &tool_ctx,
                                pre_context: pre_context.as_ref(),
                                max_turns: 30,
                                llm_timeout_secs: 60,
                                stream_timeout_secs: 120,
                                tool_timeout_secs: 30,
                                output: &mut sink,
                                user_id: &user_id_task,
                                model: &model_task,
                                log_writer: Some(log_writer_task),
                            }).await;

                            if !result.completed {
                                task_for_spawn.emit(
                                    serde_json::json!({"type": "text_delta", "content": "我已完成处理，但没有生成最终回答，请再问我一次。"})
                                        .to_string(),
                                );
                                task_for_spawn.emit(
                                    serde_json::json!({"type": "done", "usage": {"prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0}})
                                        .to_string(),
                                );
                            }
                            task_for_spawn.set_final_history(task_history);
                        });
                        task.set_abort_handle(join.abort_handle());
                    }
                    Some(AsrEvent::Error(e)) => {
                        log_stage(&state, &user_id, &conv, call_epoch, "asr_error", serde_json::json!({
                            "message": e,
                        }));
                        send_ev(&mut ws_sender, serde_json::json!({
                            "type": "error", "message": format!("语音识别错误: {}", e),
                        })).await;
                    }
                    Some(AsrEvent::Closed) | None => {
                        log_stage(&state, &user_id, &conv, call_epoch, "asr_closed", serde_json::json!({}));
                        send_ev(&mut ws_sender, serde_json::json!({
                            "type": "error", "message": "语音识别连接已断开",
                        })).await;
                        break;
                    }
                    Some(AsrEvent::SpeechStopped) => {}
                }
            }
            // ── TTS audio / lifecycle ──
            ev = async {
                match tts_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                match ev {
                    Some(TtsEvent::AudioDelta(pcm)) => {
                        tts_trace.bytes += pcm.len() as u64;
                        if !tts_trace.first_audio_logged {
                            tts_trace.first_audio_logged = true;
                            if let Some(at) = tts_trace.connect_at {
                                log_stage(&state, &user_id, &conv, call_epoch, "tts_first_audio", serde_json::json!({
                                    "ttfa_ms": at.elapsed().as_millis() as u64,
                                    "bytes": pcm.len(),
                                }));
                            }
                        }
                        send_ev(&mut ws_sender, serde_json::json!({
                            "type": "tts_audio",
                            "data": base64::engine::general_purpose::STANDARD.encode(&pcm),
                        })).await;
                    }
                    Some(TtsEvent::Error(e)) => {
                        log_stage(&state, &user_id, &conv, call_epoch, "tts_error", serde_json::json!({
                            "message": e,
                        }));
                        send_ev(&mut ws_sender, serde_json::json!({
                            "type": "error", "message": format!("语音合成错误: {}", e),
                        })).await;
                    }
                    Some(TtsEvent::Closed) | None => {
                        log_stage(&state, &user_id, &conv, call_epoch, "tts_closed", serde_json::json!({
                            "bytes": tts_trace.bytes,
                        }));
                        tts = None;
                        tts_rx = None;
                    }
                    Some(TtsEvent::ResponseDone) => {
                        log_stage(&state, &user_id, &conv, call_epoch, "tts_done", serde_json::json!({
                            "bytes": tts_trace.bytes,
                            "audio_ms": tts_trace.bytes / 48,
                            "synth_ms": tts_trace.connect_at.map(|at| at.elapsed().as_millis() as u64),
                        }));
                    }
                }
            }
            // ── Agent task events: forward to client + pipe text into TTS ──
            ev = async {
                match live_task_rx.as_mut() {
                    Some(rx) => match rx.recv().await {
                        Ok(ev) => Some(ev),
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            Some(format!("__LAGGED__:{}", n))
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            std::future::pending::<Option<String>>().await
                        }
                    },
                    None => std::future::pending::<Option<String>>().await,
                }
            } => {
                let Some(ev) = ev else { continue };
                if ev.starts_with("__LAGGED__:") {
                    // Voice audio cannot be replayed meaningfully — drop the
                    // connection so the client ends the call cleanly.
                    tracing::warn!(user_id = %user_id, conv = %conv, "Voice event receiver lagged; closing");
                    break;
                }
                let _ = ws_sender.send(Message::Text(ev.clone().into())).await;

                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&ev) {
                    match v["type"].as_str().unwrap_or("") {
                        "text_delta" => {
                            let content = v["content"].as_str().unwrap_or("");
                            if !first_text_logged {
                                first_text_logged = true;
                                if let Some(te) = turn_epoch {
                                    log_stage(&state, &user_id, &conv, call_epoch, "reply_first_text", serde_json::json!({
                                        "seq": turn_seq,
                                        "turn_ms": te.elapsed().as_millis() as u64,
                                    }));
                                }
                            }
                            for sentence in segmenter.push(content) {
                                ensure_tts(&voice_cfg, &mut tts, &mut tts_rx, &mut ws_sender, &state, &user_id, &conv, call_epoch, &mut tts_trace).await;
                                if let Some(t) = tts.as_ref() {
                                    t.append_text(&sentence);
                                }
                            }
                        }
                        "done" => {
                            log_stage(&state, &user_id, &conv, call_epoch, "turn_done", serde_json::json!({
                                "seq": turn_seq,
                                "turn_ms": turn_epoch.map(|te| te.elapsed().as_millis() as u64),
                            }));
                            if let Some(rest) = segmenter.flush() {
                                ensure_tts(&voice_cfg, &mut tts, &mut tts_rx, &mut ws_sender, &state, &user_id, &conv, call_epoch, &mut tts_trace).await;
                                if let Some(t) = tts.as_ref() {
                                    t.append_text(&rest);
                                }
                            }
                            if let Some(t) = tts.as_ref() {
                                t.finish();
                            }
                        }
                        "stopped" => {
                            log_stage(&state, &user_id, &conv, call_epoch, "turn_stopped", serde_json::json!({
                                "seq": turn_seq,
                                "turn_ms": turn_epoch.map(|te| te.elapsed().as_millis() as u64),
                            }));
                            tts = None;
                            tts_rx = None;
                            segmenter = SpeechSegmenter::new();
                            send_ev(&mut ws_sender, serde_json::json!({"type": "tts_cancel"})).await;
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    // Hang up: drop the speech sessions. A running agent task keeps running
    // (same semantics as the text chat) and its result is replayable there.
    log_stage(&state, &user_id, &conv, call_epoch, "session_end", serde_json::json!({
        "turns": turn_seq,
    }));
    drop(asr);
    drop(tts);
}