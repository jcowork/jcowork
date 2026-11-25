//! Configuration for the voice channel.
//!
//! Two backends are supported:
//! - `cloud` (default): Aliyun Model Studio (DashScope). The Omni realtime
//!   endpoint is workspace-bound, so a workspace id is required besides the
//!   API key. When the cloud is unreachable the gateway falls back to the
//!   local sidecar automatically.
//! - `local`: a sherpa-onnx sidecar (services/voice-lite) speaking the same
//!   event contract over plain WebSocket — no credentials needed.

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
    /// Preferred backend: "cloud" or "local". Cloud sessions fall back to
    /// the local sidecar when the cloud endpoint is unreachable.
    #[serde(default = "default_backend")]
    pub backend: String,
    /// Local sidecar streaming-ASR WebSocket URL.
    #[serde(default = "default_local_asr_url")]
    pub local_asr_url: String,
    /// Local sidecar streaming-TTS WebSocket URL.
    #[serde(default = "default_local_tts_url")]
    pub local_tts_url: String,
    /// Local TTS speaker id (vits-melo-tts-zh_en is multi-speaker).
    #[serde(default = "default_local_voice")]
    pub local_voice: String,
}

fn default_backend() -> String {
    "cloud".to_string()
}
fn default_local_asr_url() -> String {
    "ws://127.0.0.1:50061/asr".to_string()
}
fn default_local_tts_url() -> String {
    "ws://127.0.0.1:50061/tts".to_string()
}
fn default_local_voice() -> String {
    "0".to_string()
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
        let backend = get("JCWORK_VOICE_BACKEND")
            .map(|v| v.trim().to_ascii_lowercase())
            .filter(|v| v == "cloud" || v == "local")
            .unwrap_or_else(default_backend);
        let local_asr_url =
            get("JCWORK_VOICE_LOCAL_ASR_URL").unwrap_or_else(default_local_asr_url);
        let local_tts_url =
            get("JCWORK_VOICE_LOCAL_TTS_URL").unwrap_or_else(default_local_tts_url);
        let local_voice = get("JCWORK_VOICE_LOCAL_VOICE").unwrap_or_else(default_local_voice);
        Self {
            api_key,
            workspace_id,
            asr_model,
            tts_model,
            tts_voice,
            asr_url,
            tts_url,
            backend,
            local_asr_url,
            local_tts_url,
            local_voice,
        }
    }

    /// The cloud backend is usable only with both a key and a workspace id.
    pub fn cloud_configured(&self) -> bool {
        !self.api_key.is_empty() && !self.workspace_id.is_empty()
    }

    /// The local backend needs only the sidecar URLs (defaults are built in).
    pub fn local_configured(&self) -> bool {
        !self.local_asr_url.trim().is_empty() && !self.local_tts_url.trim().is_empty()
    }

    /// Voice calls are usable when either backend is configured: a cloud
    /// session can always fall back to the local sidecar and vice versa.
    pub fn is_configured(&self) -> bool {
        self.cloud_configured() || self.local_configured()
    }

    /// True when the admin explicitly selected the local backend.
    pub fn prefer_local(&self) -> bool {
        self.backend == "local"
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
        // No cloud credentials, but the local sidecar defaults make voice
        // usable (cloud→local fallback is always possible).
        assert!(!cfg.cloud_configured());
        assert!(cfg.local_configured());
        assert!(cfg.is_configured());
        assert_eq!(cfg.backend, "cloud");
        assert_eq!(cfg.local_asr_url, "ws://127.0.0.1:50061/asr");
        assert_eq!(cfg.local_tts_url, "ws://127.0.0.1:50061/tts");
        assert_eq!(cfg.local_voice, "0");
        assert_eq!(cfg.asr_model, "qwen3.8-omni-flash-realtime");
        assert_eq!(cfg.tts_model, "qwen-audio-3.1-tts-plus");
        assert_eq!(cfg.tts_voice, "longanlingxin_v3.1");

        // Key alone is not enough for the cloud — the Omni endpoint is
        // workspace-bound.
        let cfg = resolve_with(&[("DASHSCOPE_API_KEY", "sk-test")]);
        assert!(!cfg.cloud_configured());

        let cfg = resolve_with(&[
            ("DASHSCOPE_API_KEY", "sk-test"),
            ("JCWORK_VOICE_WORKSPACE_ID", "ws-123"),
        ]);
        assert!(cfg.cloud_configured());
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

    #[test]
    fn backend_selection_and_local_overrides() {
        let cfg = resolve_with(&[
            ("JCWORK_VOICE_BACKEND", "LOCAL"),
            ("JCWORK_VOICE_LOCAL_ASR_URL", "ws://192.168.1.5:50061/asr"),
            ("JCWORK_VOICE_LOCAL_TTS_URL", "ws://192.168.1.5:50061/tts"),
            ("JCWORK_VOICE_LOCAL_VOICE", "3"),
        ]);
        assert!(cfg.prefer_local());
        assert!(cfg.is_configured());
        assert_eq!(cfg.local_asr_url, "ws://192.168.1.5:50061/asr");
        assert_eq!(cfg.local_voice, "3");

        // Unknown backend values fall back to cloud.
        let cfg = resolve_with(&[("JCWORK_VOICE_BACKEND", "bogus")]);
        assert_eq!(cfg.backend, "cloud");

        // Cleared local URLs with no cloud credentials → unusable.
        let cfg = resolve_with(&[
            ("JCWORK_VOICE_LOCAL_ASR_URL", " "),
            ("JCWORK_VOICE_LOCAL_TTS_URL", " "),
        ]);
        assert!(!cfg.is_configured());
    }

    #[test]
    fn stored_json_without_backend_fields_still_parses() {
        // Configs persisted before the local backend existed must keep
        // working: serde defaults fill the new fields.
        let legacy = r#"{"api_key":"sk-x","workspace_id":"ws-1","asr_model":"a","tts_model":"t","tts_voice":"v","asr_url":"wss://a","tts_url":"wss://t"}"#;
        let cfg: VoiceConfig = serde_json::from_str(legacy).unwrap();
        assert_eq!(cfg.backend, "cloud");
        assert_eq!(cfg.local_asr_url, "ws://127.0.0.1:50061/asr");
        assert!(cfg.cloud_configured());
    }
}