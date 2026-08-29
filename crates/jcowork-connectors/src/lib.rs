//! Jcowork Connectors - user-managed external tool integrations.
//!
//! Users can attach "connectors" that expose tools to the agent:
//! - API connectors: manually defined HTTP tools with descriptions and
//!   parameter schemas.
//! - MCP connectors: Model Context Protocol servers (stdio or HTTP) whose
//!   tools are auto-discovered.
//!
//! Enabled tools are synced into the shared [`jcowork_tools::registry::ToolRegistry`]
//! so the LLM can select them by name/description; calls are dispatched back
//! through [`manager::ConnectorManager`].

pub mod api_executor;
pub mod manager;
pub mod mcp_client;
pub mod models;
pub mod store;

pub use manager::{ConnectorManager, ConnectorTool};
pub use models::{
    ApiConnectorConfig, ApiToolDef, Connector, ConnectorToolInfo, McpConfig, TYPE_API, TYPE_MCP,
};
pub use store::ConnectorStore;
