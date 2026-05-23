//! Jcowork Logs - structured logging for LLM requests and tool calls.

mod models;
mod writer;

pub use models::{build_llm_input, build_llm_output, ContextMessage, LlmInput, LlmLogOutput, LogEntry, ToolCallEntry};
pub use writer::LogWriter;
