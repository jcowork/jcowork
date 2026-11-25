//! Jcowork Voice — full-duplex speech channel clients.
//!
//! Wraps the Qwen realtime WebSocket APIs used by the voice call feature:
//! - Qwen3.8-Omni-Flash-Realtime for streaming ASR (server VAD + transcription)
//! - Qwen-Audio-3.1-TTS-Plus Realtime for streaming text-to-speech
//!
//! The agent answer itself is produced by the regular agent loop; this crate
//! only handles the speech-in / speech-out transports and the sentence
//! segmentation that feeds the TTS stream.

pub mod config;
pub mod omni_asr;
pub mod segment;
pub mod tts;

pub use config::VoiceConfig;
pub use omni_asr::{AsrEvent, OmniAsrClient};
pub use segment::SpeechSegmenter;
pub use tts::{TtsClient, TtsEvent};