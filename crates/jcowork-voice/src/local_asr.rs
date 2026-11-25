//! Local streaming-ASR client for the sherpa-onnx sidecar (services/voice-lite).
//!
//! Speaks the plain WebSocket contract from docs/LOCAL_VOICE_DEPLOYMENT.md
//! §4.1: binary 16 kHz s16le mono PCM frames upstream, JSON lifecycle and
//! transcription events downstream. Maps onto the same [`AsrEvent`] enum as
//! the cloud Omni client so the gateway orchestration is backend-agnostic.

use anyhow::{Context, Result};
use futures::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

use crate::omni_asr::AsrEvent;

/// Handle to a running local ASR session. Dropping it closes the connection.
pub struct LocalAsrClient {
    cmd_tx: mpsc::Sender<Message>,
}

impl LocalAsrClient {
    /// Connect to the sidecar `/asr` route; returns the handle plus the
    /// event stream. The caller must keep draining the receiver.
    pub async fn connect(url: &str) -> Result<(Self, mpsc::Receiver<AsrEvent>)> {
        let (ws, _resp) = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            tokio_tungstenite::connect_async(url),
        )
        .await
        .context("local ASR connect timeout")?
        .context("connect local ASR sidecar")?;
        let (mut sink, mut stream) = ws.split();

        let (cmd_tx, mut cmd_rx) = mpsc::channel::<Message>(256);
        let (event_tx, event_rx) = mpsc::channel::<AsrEvent>(256);

        // Writer: forward mic PCM frames verbatim (no base64, no rewrap).
        tokio::spawn(async move {
            while let Some(msg) = cmd_rx.recv().await {
                if sink.send(msg).await.is_err() {
                    break;
                }
            }
            let _ = sink.close().await;
        });

        // Reader: map sidecar JSON events onto the shared AsrEvent contract.
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
                    "ready" => Some(AsrEvent::Ready),
                    "speech_started" => Some(AsrEvent::SpeechStarted),
                    "speech_stopped" => Some(AsrEvent::SpeechStopped),
                    "partial" => Some(AsrEvent::TranscriptDelta {
                        text: v["text"].as_str().unwrap_or("").to_string(),
                        stash: v["stash"].as_str().unwrap_or("").to_string(),
                    }),
                    "final" => Some(AsrEvent::TranscriptCompleted {
                        transcript: v["text"].as_str().unwrap_or("").to_string(),
                    }),
                    "error" => Some(AsrEvent::Error(
                        v["message"].as_str().unwrap_or("unknown local ASR error").to_string(),
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
    /// are droppable under backpressure, same as the cloud client).
    pub fn append_audio(&self, pcm: &[u8]) {
        if pcm.is_empty() {
            return;
        }
        let _ = self.cmd_tx.try_send(Message::Binary(pcm.to_vec().into()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spawn a mock sidecar that plays a scripted event list, then echo the
    /// first upstream frame back as JSON so the test can assert forwarding.
    async fn mock_server(
        events: Vec<serde_json::Value>,
    ) -> (String, mpsc::Receiver<Message>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = mpsc::channel(16);
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            for ev in events {
                ws.send(Message::Text(ev.to_string().into())).await.unwrap();
            }
            // Capture one upstream frame, then close.
            if let Some(Ok(msg)) = ws.next().await {
                let _ = tx.send(msg).await;
            }
            let _ = ws.close(None).await;
        });
        (format!("ws://{}/asr", addr), rx)
    }

    #[tokio::test]
    async fn maps_sidecar_events_and_forwards_binary_pcm() {
        let (url, mut upstream) = mock_server(vec![
            serde_json::json!({"type": "ready"}),
            serde_json::json!({"type": "speech_started"}),
            serde_json::json!({"type": "partial", "text": "今天", "stash": "天气"}),
            serde_json::json!({"type": "final", "text": "今天天气怎么样"}),
        ])
        .await;
        let (client, mut rx) = LocalAsrClient::connect(&url).await.unwrap();
        assert!(matches!(rx.recv().await, Some(AsrEvent::Ready)));
        assert!(matches!(rx.recv().await, Some(AsrEvent::SpeechStarted)));
        match rx.recv().await {
            Some(AsrEvent::TranscriptDelta { text, stash }) => {
                assert_eq!(text, "今天");
                assert_eq!(stash, "天气");
            }
            other => panic!("unexpected event: {:?}", other),
        }
        match rx.recv().await {
            Some(AsrEvent::TranscriptCompleted { transcript }) => {
                assert_eq!(transcript, "今天天气怎么样");
            }
            other => panic!("unexpected event: {:?}", other),
        }

        // Mic PCM goes upstream as a raw binary frame.
        client.append_audio(&[1, 2, 3, 4]);
        match upstream.recv().await {
            Some(Message::Binary(b)) => assert_eq!(b.as_ref(), &[1, 2, 3, 4]),
            other => panic!("unexpected upstream frame: {:?}", other),
        }
        assert!(matches!(rx.recv().await, Some(AsrEvent::Closed)));
    }

    #[tokio::test]
    async fn unreachable_sidecar_fails_fast() {
        // Port 1 is reserved and nothing listens there.
        let err = LocalAsrClient::connect("ws://127.0.0.1:1/asr").await;
        assert!(err.is_err());
    }
}