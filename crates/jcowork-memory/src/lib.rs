//! Jcowork Memory - Persistent memory system with provider architecture.

pub mod builtin;
pub mod manager;
pub mod models;
pub mod provider;

pub use builtin::BuiltinMemoryProvider;
pub use manager::MemoryManager;
pub use models::{MemoryEntry, MemorySearchResult};
pub use provider::MemoryProvider;
