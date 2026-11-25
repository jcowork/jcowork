//! Backend-agnostic speech sessions with cloud→local automatic fallback.
//!
//! The gateway orchestrates against these enums instead of the concrete
//! clients, so both backends surface the identical `AsrEvent` / `TtsEvent`
//! contracts. When the preferred backend is the cloud and its endpoint is
//! unreachable (network down, bad credentials, service outage), the session
//! transparently reconnects to the local sherpa-onnx sidecar instead.

use anyhow::Result;
use tokio::sync::mpsc;

use crate::config::VoiceConfig;
use crate::local_asr::LocalAsrClient;
use crate::local_tts::LocalTtsClient;
use crate::omni_asr::{AsrEvent, OmniAsrClient};
use crate::tts::{TtsClient, TtsEvent};

/// Cloud connects get a bounded wait so a hung endpoint cannot stall the
/// fallback path; local connects carry their own shorter timeout.
const CLOUD_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(8);

/// A live ASR session on either backend.
pub enum AsrSession {
    Cloud(OmniAsrClient),
    Local(LocalAsrClient),
}

impl AsrSession {
    pub fn backend(&self) -> &'static str {
        match self {
            Self::Cloud(_) => "cloud",
            Self::Local(_) => "local",
        }
    }

    pub fn append_audio(&self, pcm: &[u8]) {
        match self {
            Self::Cloud(c) => c.append_audio(pcm),
            Self::Local(c) => c.append_audio(pcm),
        }
    }

    /// Connect strictly to the configured backend (no fallback).
    pub async fn connect(cfg: &VoiceConfig) -> Result<(Self, mpsc::Receiver<AsrEvent>)> {
        if cfg.prefer_local() {
            let (c, rx) = LocalAsrClient::connect(&cfg.local_asr_url).await?;
            Ok((Self::Local(c), rx))
        } else {
            let (c, rx) = tokio::time::timeout(CLOUD_CONNECT_TIMEOUT, OmniAsrClient::connect(cfg))
                .await
                .map_err(|_| anyhow::anyhow!("cloud ASR connect timeout"))??;
            Ok((Self::Cloud(c), rx))
        }
    }

    /// Connect to the configured backend; a cloud failure automatically
    /// retries on the local sidecar. Returns `fell_back = true` when the
    /// local backend was used despite a cloud preference.
    pub async fn connect_with_fallback(
        cfg: &VoiceConfig,
    ) -> Result<(Self, mpsc::Receiver<AsrEvent>, bool)> {
        match Self::connect(cfg).await {
            Ok((s, rx)) => Ok((s, rx, false)),
            Err(cloud_err) => {
                if cfg.prefer_local() || !cfg.local_configured() {
                    return Err(cloud_err);
                }
                let (c, rx) = LocalAsrClient::connect(&cfg.local_asr_url)
                    .await
                    .map_err(|local_err| {
                        anyhow::anyhow!(
                            "云端语音识别不可用（{}），本地服务也连接失败（{}）",
                            cloud_err,
                            local_err
                        )
                    })?;
                Ok((Self::Local(c), rx, true))
            }
        }
    }
}

/// A live TTS session on either backend.
pub enum TtsSession {
    Cloud(TtsClient),
    Local(LocalTtsClient),
}

impl TtsSession {
    pub fn backend(&self) -> &'static str {
        match self {
            Self::Cloud(_) => "cloud",
            Self::Local(_) => "local",
        }
    }

    pub fn append_text(&self, text: &str) {
        match self {
            Self::Cloud(c) => c.append_text(text),
            Self::Local(c) => c.append_text(text),
        }
    }

    pub fn finish(&self) {
        match self {
            Self::Cloud(c) => c.finish(),
            Self::Local(c) => c.finish(),
        }
    }

    /// Connect strictly to one backend. `force_local` pins the session to the
    /// sidecar (used after a cloud TTS session failed mid-call).
    pub async fn connect(
        cfg: &VoiceConfig,
        force_local: bool,
    ) -> Result<(Self, mpsc::Receiver<TtsEvent>)> {
        if cfg.prefer_local() || force_local {
            let (c, rx) = LocalTtsClient::connect(&cfg.local_tts_url, &cfg.local_voice).await?;
            Ok((Self::Local(c), rx))
        } else {
            let (c, rx) = tokio::time::timeout(CLOUD_CONNECT_TIMEOUT, TtsClient::connect(cfg))
                .await
                .map_err(|_| anyhow::anyhow!("cloud TTS connect timeout"))??;
            Ok((Self::Cloud(c), rx))
        }
    }

    /// Cloud-preferred connect with automatic local fallback on failure.
    pub async fn connect_with_fallback(
        cfg: &VoiceConfig,
        force_local: bool,
    ) -> Result<(Self, mpsc::Receiver<TtsEvent>, bool)> {
        match Self::connect(cfg, force_local).await {
            Ok((s, rx)) => Ok((s, rx, false)),
            Err(cloud_err) => {
                if cfg.prefer_local() || force_local || !cfg.local_configured() {
                    return Err(cloud_err);
                }
                let (c, rx) = LocalTtsClient::connect(&cfg.local_tts_url, &cfg.local_voice)
                    .await
                    .map_err(|local_err| {
                        anyhow::anyhow!(
                            "云端语音合成不可用（{}），本地服务也连接失败（{}）",
                            cloud_err,
                            local_err
                        )
                    })?;
                Ok((Self::Local(c), rx, true))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    fn cfg_with_cloud_dead(local_url: &str) -> VoiceConfig {
        let mut cfg = VoiceConfig::from_env();
        cfg.api_key = "sk-test".into();
        cfg.workspace_id = "ws-test".into();
        // Nothing listens on port 1 — the cloud connect fails immediately.
        cfg.asr_url = "ws://127.0.0.1:1/realtime".into();
        cfg.tts_url = "ws://127.0.0.1:1/inference".into();
        cfg.backend = "cloud".into();
        cfg.local_asr_url = local_url.into();
        cfg.local_tts_url = local_url.into();
        cfg
    }

    /// Minimal WS endpoint that completes the handshake and stays open.
    async fn silent_endpoint() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            let _ = ws.next().await; // hold until the client drops
        });
        format!("ws://{}/", addr)
    }

    #[tokio::test]
    async fn cloud_failure_falls_back_to_local_asr() {
        let url = silent_endpoint().await;
        let cfg = cfg_with_cloud_dead(&url);
        let (session, _rx, fell_back) = AsrSession::connect_with_fallback(&cfg).await.unwrap();
        assert!(fell_back);
        assert_eq!(session.backend(), "local");
    }

    #[tokio::test]
    async fn cloud_failure_falls_back_to_local_tts() {
        let url = silent_endpoint().await;
        let cfg = cfg_with_cloud_dead(&url);
        let (session, _rx, fell_back) = TtsSession::connect_with_fallback(&cfg, false)
            .await
            .unwrap();
        assert!(fell_back);
        assert_eq!(session.backend(), "local");
    }

    #[tokio::test]
    async fn explicit_local_does_not_fall_back_to_cloud() {
        let mut cfg = cfg_with_cloud_dead("ws://127.0.0.1:1/asr");
        cfg.backend = "local".into();
        assert!(AsrSession::connect_with_fallback(&cfg).await.is_err());
        assert!(TtsSession::connect_with_fallback(&cfg, false).await.is_err());
    }

    #[tokio::test]
    async fn force_local_pins_tts_to_sidecar() {
        let url = silent_endpoint().await;
        let cfg = cfg_with_cloud_dead(&url);
        let (session, _rx, fell_back) = TtsSession::connect_with_fallback(&cfg, true)
            .await
            .unwrap();
        assert!(!fell_back);
        assert_eq!(session.backend(), "local");
    }

    #[tokio::test]
    async fn both_backends_down_reports_combined_error() {
        let cfg = cfg_with_cloud_dead("ws://127.0.0.1:1/asr");
        let msg = match AsrSession::connect_with_fallback(&cfg).await {
            Err(e) => e.to_string(),
            Ok(_) => panic!("expected both backends to fail"),
        };
        assert!(msg.contains("云端"), "msg: {}", msg);
        assert!(msg.contains("本地"), "msg: {}", msg);
    }
}