//! Jcowork LLM - LLM provider abstraction and streaming.

pub mod openai;
pub mod provider;
pub mod router;

pub use openai::OpenAiConfig;
pub use provider::LlmProvider;
pub use router::{LlmRouter, ProviderConfig, ProviderInfo, ModelInfo};
