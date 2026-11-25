//! Local streaming-TTS client for the sherpa-onnx sidecar (services/voice-lite).
//!
//! Speaks the plain WebSocket contract from docs/LOCAL_VOICE_DEPLOYMENT.md
//! §4.2: JSON `start` / `text` / `finish` messages upstream, binary 24 kHz
//! s16le mono PCM frames plus `done` / `error` JSON downstream. Maps onto the
//! same [`TtsEvent`] enum as the cloud client; barge-in keeps the existing
//! semantics of dropping the whole connection.

use anyhow::{Context, Result};
use futures::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

use crate::tts::TtsEvent;

/// Handle to a running local TTS session. Dropping it closes the connection
/// (which is also how barge-in cancellation works).
pub struct LocalTtsClient {
    cmd_tx: mpsc::Sender<String>,
}

impl LocalTtsClient {
    /// Connect to the sidecar `/tts` route and open a synthesis session with
    /// the given speaker id. The sidecar processes messages in order, so no
    /// started-handshake buffering is needed (unlike the cloud protocol).
    pub async fn connect(url: &str, voice: &str) -> Result<(Self, mpsc::Receiver<TtsEvent>)> {
        let (ws, _resp) = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            tokio_tungstenite::connect_async(url),
        )
        .await
        .context("local TTS connect timeout")?
        .context("connect local TTS sidecar")?;
        let (mut sink, mut stream) = ws.split();

        let (cmd_tx, mut cmd_rx) = mpsc::channel::<String>(256);
        let (event_tx, event_rx) = mpsc::channel::<TtsEvent>(256);

        let start = serde_json::json!({"type": "start", "voice": voice, "speed": 1.0});
        let _ = cmd_tx.try_send(start.to_string());

        tokio::spawn(async move {
            while let Some(cmd) = cmd_rx.recv().await {
                if sink.send(Message::Text(cmd.into())).await.is_err() {
                    break;
                }
            }
            let _ = sink.close().await;
        });

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
                        // Same convention as the cloud client: a normal
                        // closure (1000) is benign lifecycle; anything else
                        // must surface as an error.
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
                                .send(TtsEvent::Error(format!(
                                    "本地语音合成连接被关闭: {}",
                                    reason
                                )))
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
                let ev = match v["type"].as_str().unwrap_or("") {
                    "done" => Some(TtsEvent::ResponseDone),
                    "error" => Some(TtsEvent::Error(
                        v["message"].as_str().unwrap_or("unknown local TTS error").to_string(),
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

        Ok((Self { cmd_tx }, event_rx))
    }

    /// Append one sentence of the spoken draft; the sidecar synthesizes it
    /// and streams the PCM back immediately.
    pub fn append_text(&self, text: &str) {
        if text.is_empty() {
            return;
        }
        let msg = serde_json::json!({"type": "text", "text": text});
        let _ = self.cmd_tx.try_send(msg.to_string());
    }

    /// No more text: the sidecar drains its queue and emits `done`.
    pub fn finish(&self) {
        let _ = self
            .cmd_tx
            .try_send(serde_json::json!({"type": "finish"}).to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn start_text_finish_protocol_and_audio_mapping() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (up_tx, mut up_rx) = mpsc::channel::<Message>(16);
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            // Collect start/text/finish, then answer with audio + done.
            let mut got = Vec::new();
            while got.len() < 3 {
                match ws.next().await {
                    Some(Ok(Message::Text(t))) => got.push(t.to_string()),
                    _ => break,
                }
            }
            for g in &got {
                let _ = up_tx.send(Message::Text(g.clone().into())).await;
            }
            ws.send(Message::Binary(vec![9, 8, 7, 6].into())).await.unwrap();
            ws.send(Message::Text(
                serde_json::json!({"type": "done"}).to_string().into(),
            ))
            .await
            .unwrap();
            let _ = ws.close(None).await;
        });

        let url = format!("ws://{}/tts", addr);
        let (client, mut rx) = LocalTtsClient::connect(&url, "2").await.unwrap();
        client.append_text("你好，世界。");
        client.finish();

        // Upstream: start carries the speaker id, then text, then finish.
        let mut upstream = Vec::new();
        for _ in 0..3 {
            if let Some(Message::Text(t)) = up_rx.recv().await {
                upstream.push(serde_json::from_str::<serde_json::Value>(&t).unwrap());
            }
        }
        assert_eq!(upstream[0]["type"], "start");
        assert_eq!(upstream[0]["voice"], "2");
        assert_eq!(upstream[1]["type"], "text");
        assert_eq!(upstream[1]["text"], "你好，世界。");
        assert_eq!(upstream[2]["type"], "finish");

        // Downstream: binary PCM → AudioDelta, done → ResponseDone.
        match rx.recv().await {
            Some(TtsEvent::AudioDelta(pcm)) => assert_eq!(pcm, vec![9, 8, 7, 6]),
            other => panic!("unexpected event: {:?}", other),
        }
        assert!(matches!(rx.recv().await, Some(TtsEvent::ResponseDone)));
        assert!(matches!(rx.recv().await, Some(TtsEvent::Closed)));
    }

    #[tokio::test]
    async fn abnormal_close_surfaces_as_error() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            let frame = tokio_tungstenite::tungstenite::protocol::CloseFrame {
                code: 1007u16.into(),
                reason: "bad voice".into(),
            };
            let _ = ws.close(Some(frame)).await;
        });
        let url = format!("ws://{}/tts", addr);
        let (_client, mut rx) = LocalTtsClient::connect(&url, "0").await.unwrap();
        match rx.recv().await {
            Some(TtsEvent::Error(e)) => assert!(e.contains("1007")),
            other => panic!("unexpected event: {:?}", other),
        }
    }
}