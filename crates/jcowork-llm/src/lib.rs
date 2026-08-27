//! Jcowork LLM - LLM provider abstraction and streaming.

pub mod openai;
pub mod provider;
pub mod router;

pub use openai::OpenAiConfig;
pub use provider::LlmProvider;
#[cfg(any(test, feature = "test-utils"))]
pub use provider::MockLlmProvider;
pub use router::{LlmRouter, ProviderConfig, ProviderEntry, ProviderInfo, ModelInfo};
