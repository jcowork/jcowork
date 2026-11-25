//! Environment-driven configuration for the voice channel.
//!
//! Both speech models live on Aliyun Model Studio (DashScope). The Omni
//! realtime endpoint is workspace-bound, so a workspace id is required in
//! addition to the API key.

/// Voice channel configuration resolved from environment variables.
///
/// Serializable so an admin-managed copy can live in `app_settings` and
/// override the environment at startup (same pattern as the mail channel).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct VoiceConfig {
    /// DashScope (百炼) API key; falls back to QWEN_API_KEY.
    pub api_key: String,
    /// Model Studio workspace id (required by the Omni realtime endpoint).
    pub workspace_id: String,
    pub asr_model: String,
    pub tts_model: String,
    pub tts_voice: String,
    /// Full ASR WebSocket URL (including ?model=).
    pub asr_url: String,
    /// TTS WebSocket URL (native inference endpoint; the model travels in
    /// the run-task payload, not the URL).
    pub tts_url: String,
}

impl VoiceConfig {
    pub fn from_env() -> Self {
        Self::resolve(|key| std::env::var(key).ok().filter(|v| !v.trim().is_empty()))
    }

    /// Pure resolver so tests can inject values without touching the process
    /// environment (env-var tests are flaky when run in parallel).
    fn resolve(get: impl Fn(&str) -> Option<String>) -> Self {
        let api_key = get("DASHSCOPE_API_KEY")
            .or_else(|| get("QWEN_API_KEY"))
            .unwrap_or_default();
        let workspace_id = get("JCWORK_VOICE_WORKSPACE_ID").unwrap_or_default();
        let asr_model =
            get("JCWORK_VOICE_ASR_MODEL").unwrap_or_else(|| "qwen3.8-omni-flash-realtime".into());
        let tts_model =
            get("JCWORK_VOICE_TTS_MODEL").unwrap_or_else(|| "qwen-audio-3.1-tts-plus".into());
        let tts_voice =
            get("JCWORK_VOICE_TTS_VOICE").unwrap_or_else(|| "longanlingxin_v3.1".into());
        let asr_url = get("JCWORK_VOICE_ASR_URL")
            .unwrap_or_else(|| Self::default_asr_url(&workspace_id, &asr_model));
        let tts_url = get("JCWORK_VOICE_TTS_URL").unwrap_or_else(Self::default_tts_url);
        Self {
            api_key,
            workspace_id,
            asr_model,
            tts_model,
            tts_voice,
            asr_url,
            tts_url,
        }
    }

    /// The voice feature is usable only with both a key and a workspace id.
    pub fn is_configured(&self) -> bool {
        !self.api_key.is_empty() && !self.workspace_id.is_empty()
    }

    /// Workspace-bound Omni realtime endpoint (Beijing region).
    pub fn default_asr_url(workspace_id: &str, asr_model: &str) -> String {
        format!(
            "wss://{}.cn-beijing.maas.aliyuncs.com/api-ws/v1/realtime?model={}",
            workspace_id, asr_model
        )
    }

    /// Native DashScope duplex inference endpoint used by the Qwen-Audio-TTS
    /// family (run-task / continue-task / finish-task protocol).
    pub fn default_tts_url() -> String {
        "wss://dashscope.aliyuncs.com/api-ws/v1/inference".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::VoiceConfig;
    use std::collections::HashMap;

    fn resolve_with(pairs: &[(&str, &str)]) -> VoiceConfig {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        VoiceConfig::resolve(|key| map.get(key).cloned())
    }

    #[test]
    fn defaults_and_configuration_gate() {
        let cfg = resolve_with(&[]);
        assert!(!cfg.is_configured());
        assert_eq!(cfg.asr_model, "qwen3.8-omni-flash-realtime");
        assert_eq!(cfg.tts_model, "qwen-audio-3.1-tts-plus");
        assert_eq!(cfg.tts_voice, "longanlingxin_v3.1");

        // Key alone is not enough — the Omni endpoint is workspace-bound.
        let cfg = resolve_with(&[("DASHSCOPE_API_KEY", "sk-test")]);
        assert!(!cfg.is_configured());

        let cfg = resolve_with(&[
            ("DASHSCOPE_API_KEY", "sk-test"),
            ("JCWORK_VOICE_WORKSPACE_ID", "ws-123"),
        ]);
        assert!(cfg.is_configured());
        assert_eq!(
            cfg.asr_url,
            "wss://ws-123.cn-beijing.maas.aliyuncs.com/api-ws/v1/realtime?model=qwen3.8-omni-flash-realtime"
        );
        assert_eq!(
            cfg.tts_url,
            "wss://dashscope.aliyuncs.com/api-ws/v1/inference"
        );
    }

    #[test]
    fn key_fallback_and_overrides() {
        let cfg = resolve_with(&[
            ("QWEN_API_KEY", "sk-qwen"),
            ("JCWORK_VOICE_WORKSPACE_ID", "ws-123"),
            ("JCWORK_VOICE_ASR_MODEL", "custom-asr"),
            ("JCWORK_VOICE_TTS_VOICE", "Cherry"),
            ("JCWORK_VOICE_ASR_URL", "wss://example.com/asr"),
            ("JCWORK_VOICE_TTS_URL", "wss://example.com/tts"),
        ]);
        assert!(cfg.is_configured());
        assert_eq!(cfg.api_key, "sk-qwen");
        assert_eq!(cfg.asr_url, "wss://example.com/asr");
        assert_eq!(cfg.tts_url, "wss://example.com/tts");
        assert_eq!(cfg.tts_voice, "Cherry");

        // DASHSCOPE_API_KEY wins over the QWEN_API_KEY fallback.
        let cfg = resolve_with(&[
            ("QWEN_API_KEY", "sk-qwen"),
            ("DASHSCOPE_API_KEY", "sk-dash"),
            ("JCWORK_VOICE_WORKSPACE_ID", "ws-123"),
        ]);
        assert_eq!(cfg.api_key, "sk-dash");
    }
}