//! Jcowork Agent - Core agent loop with prompt building and context compression.

pub mod context;
pub mod delegate;
pub mod prompt;
pub mod r#loop;

pub use context::ContextEngine;
pub use r#loop::AgentLoop;
pub use prompt::PromptBuilder;
