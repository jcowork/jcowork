//! Qwen3.8-Omni-Flash-Realtime client used as a streaming ASR engine.
//!
//! The session runs with server VAD and input transcription enabled. We only
//! consume the transcription events — the model's own answer generation is
//! cancelled as soon as it starts (`response.cancel` on `response.created`),
//! because the reply is produced by the jcowork agent loop instead.

use anyhow::{Context, Result};
use base64::Engine;
use futures::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

use crate::config::VoiceConfig;

/// Events surfaced to the voice gateway.
#[derive(Debug)]
pub enum AsrEvent {
    /// Session configured and ready to accept audio.
    Ready,
    /// Server VAD detected speech start (barge-in trigger).
    SpeechStarted,
    SpeechStopped,
    /// Streaming transcription preview: confirmed prefix + tentative suffix.
    TranscriptDelta { text: String, stash: String },
    /// Final transcript of one utterance.
    TranscriptCompleted { transcript: String },
    Error(String),
    /// The upstream connection closed.
    Closed,
}

/// Handle to a running Omni ASR session. Dropping it closes the connection.
pub struct OmniAsrClient {
    cmd_tx: mpsc::Sender<String>,
}

impl OmniAsrClient {
    /// Connect and configure the session; returns the handle plus the event
    /// stream. The caller must keep draining the receiver.
    pub async fn connect(cfg: &VoiceConfig) -> Result<(Self, mpsc::Receiver<AsrEvent>)> {
        let mut req = cfg
            .asr_url
            .as_str()
            .into_client_request()
            .context("invalid ASR WebSocket URL")?;
        req.headers_mut().insert(
            "Authorization",
            format!("Bearer {}", cfg.api_key)
                .parse()
                .context("invalid ASR API key header")?,
        );
        let (ws, _resp) = tokio_tungstenite::connect_async(req)
            .await
            .context("connect Omni ASR realtime endpoint")?;
        let (mut sink, mut stream) = ws.split();

        let (cmd_tx, mut cmd_rx) = mpsc::channel::<String>(256);
        let (event_tx, event_rx) = mpsc::channel::<AsrEvent>(256);

        // Configure: text-only output, server VAD, streaming transcription,
        // 16 kHz s16le mono PCM input.
        let session_update = serde_json::json!({
            "type": "session.update",
            "session": {
                "modalities": ["text"],
                "audio": {
                    "input": {"format": {
                        "type": "pcm", "sample_rate": 16000,
                        "sample_format": "s16le", "channels": 1,
                        "packing": "interleaved", "channel_layout": "mono",
                    }},
                },
                "input_audio_transcription": {"model": "qwen3-asr-flash-realtime"},
                "turn_detection": {
                    "type": "server_vad",
                    "threshold": 0.1,
                    "prefix_padding_ms": 500,
                    "silence_duration_ms": 700,
                },
            },
        });
        let _ = cmd_tx.try_send(session_update.to_string());

        // Writer: forward queued client events to the socket.
        tokio::spawn(async move {
            while let Some(cmd) = cmd_rx.recv().await {
                if sink.send(Message::Text(cmd.into())).await.is_err() {
                    break;
                }
            }
            let _ = sink.close().await;
        });

        // Reader: map server events to AsrEvent; suppress the model's own
        // responses so only the transcription is used.
        let cancel_tx = cmd_tx.clone();
        tokio::spawn(async move {
            while let Some(msg) = stream.next().await {
                let text = match msg {
                    Ok(Message::Text(t)) => t,
                    Ok(Message::Close(_)) => break,
                    Ok(_) => continue,
                    Err(e) => {
                        let _ = event_tx.send(AsrEvent::Error(e.to_string())).await;
                        break;
                    }
                };
                let v: serde_json::Value = match serde_json::from_str(&text) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let ev = match v["type"].as_str().unwrap_or("") {
                    "session.updated" => Some(AsrEvent::Ready),
                    "input_audio_buffer.speech_started" => Some(AsrEvent::SpeechStarted),
                    "input_audio_buffer.speech_stopped" => Some(AsrEvent::SpeechStopped),
                    "conversation.item.input_audio_transcription.delta" => {
                        Some(AsrEvent::TranscriptDelta {
                            text: v["text"].as_str().unwrap_or("").to_string(),
                            stash: v["stash"].as_str().unwrap_or("").to_string(),
                        })
                    }
                    "conversation.item.input_audio_transcription.completed" => {
                        Some(AsrEvent::TranscriptCompleted {
                            transcript: v["transcript"].as_str().unwrap_or("").to_string(),
                        })
                    }
                    "response.created" => {
                        // The agent answers, not Omni — cancel immediately.
                        let _ = cancel_tx
                            .try_send(serde_json::json!({"type": "response.cancel"}).to_string());
                        None
                    }
                    "error" => Some(AsrEvent::Error(
                        v["error"]["message"]
                            .as_str()
                            .or_else(|| v["error"].as_str())
                            .unwrap_or("unknown ASR error")
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
            let _ = event_tx.send(AsrEvent::Closed).await;
        });

        Ok((Self { cmd_tx }, event_rx))
    }

    /// Append a chunk of 16 kHz 16-bit mono PCM (non-blocking; audio frames
    /// are droppable under backpressure).
    pub fn append_audio(&self, pcm: &[u8]) {
        if pcm.is_empty() {
            return;
        }
        let msg = serde_json::json!({
            "type": "input_audio_buffer.append",
            "audio": base64::engine::general_purpose::STANDARD.encode(pcm),
        });
        let _ = self.cmd_tx.try_send(msg.to_string());
    }
}