//! Qwen-Audio-3.1-TTS-Plus streaming client (native DashScope duplex protocol).
//!
//! The Qwen-Audio-TTS family speaks the native `/api-ws/v1/inference` duplex
//! protocol (run-task / continue-task / finish-task) — NOT the OpenAI-style
//! realtime protocol used by the qwen3-tts-*-realtime models. Text is
//! appended as it streams from the agent; PCM audio (24 kHz s16le mono)
//! arrives as binary WebSocket frames between JSON lifecycle events.
//!
//! Text sent before `task-started` is buffered and released in order once
//! the task begins. Interruption is handled by dropping the whole
//! connection — the gateway lazily reconnects for the next reply, which is
//! simpler and more reliable than rolling back partial synthesis state.

use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use futures::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

use crate::config::VoiceConfig;

/// Events surfaced to the voice gateway.
#[derive(Debug)]
pub enum TtsEvent {
    /// Raw 24 kHz 16-bit mono PCM chunk (binary WS frame).
    AudioDelta(Vec<u8>),
    /// The synthesis task finished (task-finished).
    ResponseDone,
    Error(String),
    /// The upstream connection closed.
    Closed,
}

/// Ordering state shared between append/finish and the reader task.
#[derive(Default)]
struct SessionState {
    started: bool,
    finish_requested: bool,
    pending: Vec<String>,
}

/// Handle to a running TTS session. Dropping it closes the connection
/// (which is also how barge-in cancellation works).
pub struct TtsClient {
    cmd_tx: mpsc::Sender<String>,
    state: Arc<Mutex<SessionState>>,
    task_id: String,
}

fn continue_task(task_id: &str, text: &str) -> String {
    serde_json::json!({
        "header": {"action": "continue-task", "task_id": task_id, "streaming": "duplex"},
        "payload": {"input": {"text": text}},
    })
    .to_string()
}

fn finish_task(task_id: &str) -> String {
    serde_json::json!({
        "header": {"action": "finish-task", "task_id": task_id, "streaming": "duplex"},
        "payload": {"input": {}},
    })
    .to_string()
}

impl TtsClient {
    pub async fn connect(cfg: &VoiceConfig) -> Result<(Self, mpsc::Receiver<TtsEvent>)> {
        let mut req = cfg
            .tts_url
            .as_str()
            .into_client_request()
            .context("invalid TTS WebSocket URL")?;
        req.headers_mut().insert(
            "Authorization",
            format!("Bearer {}", cfg.api_key)
                .parse()
                .context("invalid TTS API key header")?,
        );
        let (ws, _resp) = tokio_tungstenite::connect_async(req)
            .await
            .context("connect Qwen TTS inference endpoint")?;
        let (mut sink, mut stream) = ws.split();

        let (cmd_tx, mut cmd_rx) = mpsc::channel::<String>(256);
        let (event_tx, event_rx) = mpsc::channel::<TtsEvent>(256);
        let task_id = uuid::Uuid::new_v4().simple().to_string();
        let state = Arc::new(Mutex::new(SessionState::default()));

        let run_task = serde_json::json!({
            "header": {"action": "run-task", "task_id": task_id, "streaming": "duplex"},
            "payload": {
                "task_group": "audio",
                "task": "tts",
                "function": "SpeechSynthesizer",
                "model": cfg.tts_model,
                "parameters": {
                    "voice": cfg.tts_voice,
                    "format": "pcm",
                    "sample_rate": 24000,
                },
                "input": {},
            },
        });
        let _ = cmd_tx.try_send(run_task.to_string());

        tokio::spawn(async move {
            while let Some(cmd) = cmd_rx.recv().await {
                if sink.send(Message::Text(cmd.into())).await.is_err() {
                    break;
                }
            }
            let _ = sink.close().await;
        });

        let reader_state = state.clone();
        let reader_cmd = cmd_tx.clone();
        let reader_task_id = task_id.clone();
        tokio::spawn(async move {
            while let Some(msg) = stream.next().await {
                let text = match msg {
                    Ok(Message::Binary(pcm)) => {
                        if !pcm.is_empty()
                            && event_tx.send(TtsEvent::AudioDelta(pcm.to_vec())).await.is_err()
                        {
                            break;
                        }
                        continue;
                    }
                    Ok(Message::Text(t)) => t,
                    Ok(Message::Close(frame)) => {
                        // A normal closure (1000) is the server ending a
                        // finished or idle task — benign lifecycle. Any other
                        // code (e.g. 1007 invalid voice) must surface as an
                        // error, not a silent drop.
                        let normal = frame
                            .as_ref()
                            .map(|f| u16::from(f.code) == 1000)
                            .unwrap_or(true);
                        if normal {
                            let _ = event_tx.send(TtsEvent::Closed).await;
                        } else {
                            let reason = frame
                                .map(|f| format!("{} {}", f.code, f.reason))
                                .unwrap_or_else(|| "connection closed".to_string());
                            let _ = event_tx
                                .send(TtsEvent::Error(format!("语音合成连接被服务端关闭: {}", reason)))
                                .await;
                        }
                        break;
                    }
                    Ok(_) => continue,
                    Err(e) => {
                        let _ = event_tx.send(TtsEvent::Error(e.to_string())).await;
                        break;
                    }
                };
                let v: serde_json::Value = match serde_json::from_str(&text) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let ev = match v["header"]["event"].as_str().unwrap_or("") {
                    "task-started" => {
                        // Release text buffered before the task began, then
                        // honour a finish() that arrived early.
                        let mut st = reader_state.lock().unwrap();
                        st.started = true;
                        for pending in st.pending.drain(..) {
                            let _ = reader_cmd.try_send(continue_task(&reader_task_id, &pending));
                        }
                        if st.finish_requested {
                            let _ = reader_cmd.try_send(finish_task(&reader_task_id));
                        }
                        None
                    }
                    "task-finished" => Some(TtsEvent::ResponseDone),
                    "task-failed" => Some(TtsEvent::Error(
                        v["header"]["error_message"]
                            .as_str()
                            .or_else(|| v["payload"]["message"].as_str())
                            .unwrap_or("unknown TTS error")
                            .to_string(),
                    )),
                    _ => None,
                };
                if let Some(ev) = ev {
                    if event_tx.send(ev).await.is_err() {
                        break;
                    }
                }
            }
            let _ = event_tx.send(TtsEvent::Closed).await;
        });

        Ok((Self { cmd_tx, state, task_id }, event_rx))
    }

    /// Append streaming reply text; the service picks synthesis boundaries.
    pub fn append_text(&self, text: &str) {
        if text.is_empty() {
            return;
        }
        let mut st = self.state.lock().unwrap();
        if st.started {
            let _ = self.cmd_tx.try_send(continue_task(&self.task_id, text));
        } else {
            st.pending.push(text.to_string());
        }
    }

    /// No more text: the service synthesizes whatever is buffered and ends
    /// the task (task-finished), after which the connection closes.
    pub fn finish(&self) {
        let mut st = self.state.lock().unwrap();
        if st.started {
            let _ = self.cmd_tx.try_send(finish_task(&self.task_id));
        } else {
            st.finish_requested = true;
        }
    }
}