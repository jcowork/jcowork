//! Jcowork Voice — full-duplex speech channel clients.
//!
//! Two interchangeable backends behind one event contract:
//! - Cloud: Qwen3.8-Omni-Flash-Realtime (streaming ASR) + Qwen-Audio-3.1-TTS-Plus
//!   (streaming TTS) over the DashScope WebSocket APIs.
//! - Local: a sherpa-onnx sidecar (services/voice-lite) for fully offline
//!   speech-in / speech-out on CPU.
//!
//! The agent answer itself is produced by the regular agent loop; this crate
//! only handles the speech transports, backend selection with cloud→local
//! fallback, and the sentence segmentation that feeds the TTS stream.

pub mod config;
pub mod local_asr;
pub mod local_tts;
pub mod omni_asr;
pub mod segment;
pub mod session;
pub mod tts;

pub use config::VoiceConfig;
pub use local_asr::LocalAsrClient;
pub use local_tts::LocalTtsClient;
pub use omni_asr::{AsrEvent, OmniAsrClient};
pub use segment::SpeechSegmenter;
pub use session::{AsrSession, TtsSession};
pub use tts::{TtsClient, TtsEvent};