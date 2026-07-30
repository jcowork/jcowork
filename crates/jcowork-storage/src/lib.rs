//! Jcowork Storage - Database and file storage layer.

pub mod database;
pub mod doc_chunker;
pub mod embedding_client;
pub mod excel_db;
pub mod feishu_store;
pub mod file_store;
pub mod migration;
pub mod user_store;
pub mod workspace_index;

pub use database::Database;
pub use doc_chunker::{DocChunk, chunk_markdown};
pub use embedding_client::{EmbeddingClient, cosine_similarity, embedding_to_bytes, bytes_to_embedding};
pub use feishu_store::FeishuConfigStore;
pub use file_store::FileStore;
pub use user_store::UserStore;
pub use workspace_index::{WorkspaceIndex, ScoredChunk, DocChunkRow};
