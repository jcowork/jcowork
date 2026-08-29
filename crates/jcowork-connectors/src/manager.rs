//! Connector manager: bridges persisted connector configs with the tool
//! registry and the agent loop.
//!
//! Responsibilities:
//! - CRUD passthrough to [`ConnectorStore`]
//! - Syncing enabled connector tools into the dynamic area of
//!   [`ToolRegistry`] (so the LLM sees their schemas)
//! - Executing connector tool calls (API executor or MCP client)
//! - Caching MCP connections and discovered tool schemas

use anyhow::{anyhow, bail, Result};
use async_trait::async_trait;
use dashmap::DashMap;
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

use jcowork_llm::provider::{FunctionDefinition, ToolDefinition};
use jcowork_tools::base::{Tool, ToolContext};
use jcowork_tools::registry::ToolRegistry;

use crate::api_executor::{execute_api_tool, validate_api_tool};
use crate::mcp_client::{McpClient, McpTool};
use crate::models::{
    global_tool_name, parse_global_tool_name, ApiConnectorConfig, Connector, ConnectorToolInfo,
    McpConfig, TYPE_API, TYPE_MCP,
};
use crate::store::ConnectorStore;

/// Internal index entry for a registered (enabled) connector tool.
#[derive(Debug, Clone)]
struct ResolvedTool {
    user_id: String,
    connector_id: String,
    ctype: String,
    original_name: String,
}

/// Manages connector lifecycle, tool registration and execution.
pub struct ConnectorManager {
    store: ConnectorStore,
    registry: Mutex<Option<Arc<ToolRegistry>>>,
    /// Live MCP connections keyed by connector id.
    connections: DashMap<String, Arc<McpClient>>,
    /// Discovered MCP tool schemas keyed by connector id.
    schema_cache: DashMap<String, Vec<McpTool>>,
    /// Global tool name -> resolved metadata (only enabled tools).
    tool_index: DashMap<String, ResolvedTool>,
    /// Serializes sync_registry calls.
    sync_guard: Mutex<()>,
}

impl ConnectorManager {
    pub fn new(pool: SqlitePool) -> Arc<Self> {
        Arc::new(Self {
            store: ConnectorStore::new(pool),
            registry: Mutex::new(None),
            connections: DashMap::new(),
            schema_cache: DashMap::new(),
            tool_index: DashMap::new(),
            sync_guard: Mutex::new(()),
        })
    }

    /// Attach the tool registry that connector tools should be synced into.
    pub async fn attach_registry(self: &Arc<Self>, registry: Arc<ToolRegistry>) {
        *self.registry.lock().await = Some(registry);
    }

    /// Direct access to the persistence layer (for HTTP handlers).
    pub fn store(&self) -> &ConnectorStore {
        &self.store
    }

    /// Reconcile the registry's dynamic tools with the database state.
    ///
    /// Registers tools of all enabled connectors (connector-level AND
    /// tool-level) and unregisters anything that is no longer active.
    /// Must be called at startup and after every connector mutation.
    pub async fn sync_registry(self: &Arc<Self>) -> Result<()> {
        let _guard = self.sync_guard.lock().await;
        let registry = match self.registry.lock().await.clone() {
            Some(r) => r,
            None => return Ok(()), // registry not attached yet — nothing to do
        };

        let connectors = self.store.list_all_enabled().await?;
        let mut desired: HashMap<String, ResolvedTool> = HashMap::new();

        for connector in &connectors {
            match connector.ctype.as_str() {
                TYPE_API => {
                    let cfg: ApiConnectorConfig =
                        serde_json::from_value(connector.config.clone()).unwrap_or_default();
                    for tool in cfg.tools.iter().filter(|t| t.enabled) {
                        let name = global_tool_name(&connector.id, &tool.name);
                        desired.insert(
                            name,
                            ResolvedTool {
                                user_id: connector.user_id.clone(),
                                connector_id: connector.id.clone(),
                                ctype: TYPE_API.to_string(),
                                original_name: tool.name.clone(),
                            },
                        );
                    }
                }
                TYPE_MCP => {
                    match self.mcp_tools(connector).await {
                        Ok(tools) => {
                            for tool in &tools {
                                if connector.tool_enabled(&tool.name) {
                                    let name = global_tool_name(&connector.id, &tool.name);
                                    desired.insert(
                                        name,
                                        ResolvedTool {
                                            user_id: connector.user_id.clone(),
                                            connector_id: connector.id.clone(),
                                            ctype: TYPE_MCP.to_string(),
                                            original_name: tool.name.clone(),
                                        },
                                    );
                                }
                            }
                        }
                        Err(e) => {
                            // Unreachable servers must not break other connectors.
                            tracing::warn!(
                                connector_id = %connector.id,
                                name = %connector.name,
                                err = %e,
                                "Skipping MCP connector: failed to list tools"
                            );
                        }
                    }
                }
                _ => {}
            }
        }

        // Unregister tools that are no longer active.
        let stale: Vec<String> = self
            .tool_index
            .iter()
            .filter(|entry| !desired.contains_key(entry.key()))
            .map(|entry| entry.key().clone())
            .collect();
        for name in &stale {
            registry.unregister_dynamic(name);
            self.tool_index.remove(name);
        }

        // Register new or updated tools.
        for (name, resolved) in &desired {
            let meta = resolved.clone();
            let description = match meta.ctype.as_str() {
                TYPE_API => {
                    let connector = connectors
                        .iter()
                        .find(|c| c.id == meta.connector_id)
                        .cloned()
                        .unwrap_or_default();
                    let cfg: ApiConnectorConfig =
                        serde_json::from_value(connector.config).unwrap_or_default();
                    cfg.tools
                        .iter()
                        .find(|t| t.name == meta.original_name)
                        .map(|t| t.description.clone())
                        .unwrap_or_default()
                }
                TYPE_MCP => self
                    .schema_cache
                    .get(&meta.connector_id)
                    .and_then(|tools| {
                        tools
                            .iter()
                            .find(|t| t.name == meta.original_name)
                            .map(|t| t.description.clone())
                    })
                    .unwrap_or_default(),
                _ => String::new(),
            };
            let parameters = match meta.ctype.as_str() {
                TYPE_API => {
                    let connector = connectors
                        .iter()
                        .find(|c| c.id == meta.connector_id)
                        .cloned()
                        .unwrap_or_default();
                    let cfg: ApiConnectorConfig =
                        serde_json::from_value(connector.config).unwrap_or_default();
                    cfg.tools
                        .iter()
                        .find(|t| t.name == meta.original_name)
                        .map(|t| t.params.clone())
                        .unwrap_or(serde_json::json!({"type": "object"}))
                }
                TYPE_MCP => self
                    .schema_cache
                    .get(&meta.connector_id)
                    .and_then(|tools| {
                        tools
                            .iter()
                            .find(|t| t.name == meta.original_name)
                            .map(|t| t.input_schema.clone())
                    })
                    .unwrap_or(serde_json::json!({"type": "object"})),
                _ => serde_json::json!({"type": "object"}),
            };

            let tool = Arc::new(ConnectorTool {
                name: name.clone(),
                description,
                parameters,
                user_id: meta.user_id.clone(),
                manager: Arc::clone(self),
            });
            registry.register_dynamic(tool);
            self.tool_index.insert(name.clone(), meta);
        }

        tracing::info!(
            registered = desired.len(),
            removed = stale.len(),
            "Connector tools synced into registry"
        );
        Ok(())
    }

    /// Execute a connector tool by its global registry name.
    ///
    /// Only tools present in the active index (connector enabled + tool
    /// enabled) can be executed; everything else is rejected.
    pub async fn execute_tool(&self, user_id: &str, global_name: &str, args: &str) -> Result<String> {
        let resolved = self
            .tool_index
            .get(global_name)
            .map(|r| r.clone())
            .ok_or_else(|| anyhow!("Connector tool is disabled or no longer exists: {}", global_name))?;
        if resolved.user_id != user_id {
            bail!("Connector tool belongs to another user");
        }

        let connector = self.store.get(user_id, &resolved.connector_id).await?;
        if !connector.enabled {
            bail!("Connector '{}' is disabled", connector.name);
        }

        match resolved.ctype.as_str() {
            TYPE_API => {
                let cfg: ApiConnectorConfig =
                    serde_json::from_value(connector.config).unwrap_or_default();
                let tool = cfg
                    .tools
                    .into_iter()
                    .find(|t| t.name == resolved.original_name)
                    .ok_or_else(|| anyhow!("API tool '{}' no longer exists", resolved.original_name))?;
                if !tool.enabled {
                    bail!("Tool '{}' is disabled", tool.name);
                }
                execute_api_tool(&tool, args).await
            }
            TYPE_MCP => {
                if !connector.tool_enabled(&resolved.original_name) {
                    bail!("Tool '{}' is disabled", resolved.original_name);
                }
                let client = self.get_or_connect(&connector).await?;
                let arguments: serde_json::Value = if args.trim().is_empty() {
                    serde_json::json!({})
                } else {
                    serde_json::from_str(args)
                        .map_err(|e| anyhow!("Invalid tool arguments JSON: {}", e))?
                };
                client.call_tool(&resolved.original_name, arguments).await
            }
            other => Err(anyhow!("Unknown connector type: {}", other)),
        }
    }

    /// List the tools of a connector (for the frontend tools panel).
    ///
    /// API connectors are read from config; MCP connectors are connected
    /// and queried via tools/list.
    pub async fn list_tools(&self, connector: &Connector) -> Result<Vec<ConnectorToolInfo>> {
        match connector.ctype.as_str() {
            TYPE_API => {
                let cfg: ApiConnectorConfig =
                    serde_json::from_value(connector.config.clone()).unwrap_or_default();
                Ok(cfg
                    .tools
                    .into_iter()
                    .map(|t| ConnectorToolInfo {
                        name: t.name,
                        description: t.description,
                        parameters: t.params,
                        enabled: t.enabled,
                    })
                    .collect())
            }
            TYPE_MCP => {
                let tools = self.mcp_tools(connector).await?;
                Ok(tools
                    .into_iter()
                    .map(|t| ConnectorToolInfo {
                        enabled: connector.tool_enabled(&t.name),
                        name: t.name,
                        description: t.description,
                        parameters: t.input_schema,
                    })
                    .collect())
            }
            other => Err(anyhow!("Unknown connector type: {}", other)),
        }
    }

    /// Test a connector configuration without persisting it.
    ///
    /// Returns a human-readable summary on success.
    pub async fn test_connector(&self, connector: &Connector) -> Result<String> {
        match connector.ctype.as_str() {
            TYPE_API => {
                let cfg: ApiConnectorConfig =
                    serde_json::from_value(connector.config.clone()).unwrap_or_default();
                if cfg.tools.is_empty() {
                    bail!("API connector has no tools defined");
                }
                for tool in &cfg.tools {
                    validate_api_tool(tool)
                        .map_err(|e| anyhow!("Tool '{}': {}", tool.name, e))?;
                }
                Ok(format!("{} tool(s) validated", cfg.tools.len()))
            }
            TYPE_MCP => {
                let config: McpConfig = serde_json::from_value(connector.config.clone())
                    .map_err(|e| anyhow!("Invalid MCP config: {}", e))?;
                config
                    .validate()
                    .map_err(|msg| anyhow!("Invalid MCP config: {}", msg))?;
                let client = connect_mcp(&config).await?;
                client.initialize().await?;
                let tools = client.list_tools().await?;
                Ok(format!("Connected, discovered {} tool(s)", tools.len()))
            }
            other => Err(anyhow!("Unknown connector type: {}", other)),
        }
    }

    /// Drop cached MCP connection + schema cache for a connector.
    pub fn invalidate_connector(&self, connector_id: &str) {
        self.connections.remove(connector_id);
        self.schema_cache.remove(connector_id);
    }

    /// Drop all cached state for a user (called on any of their mutations).
    pub async fn invalidate_user(self: &Arc<Self>, user_id: &str) {
        if let Ok(connectors) = self.store.list(user_id).await {
            for c in connectors {
                self.invalidate_connector(&c.id);
            }
        }
    }

    /// Get (or discover) the MCP tools of a connector, with caching.
    async fn mcp_tools(&self, connector: &Connector) -> Result<Vec<McpTool>> {
        if let Some(cached) = self.schema_cache.get(&connector.id) {
            return Ok(cached.clone());
        }
        let client = self.get_or_connect(connector).await?;
        let tools = client.list_tools().await?;
        self.schema_cache
            .insert(connector.id.clone(), tools.clone());
        Ok(tools)
    }

    /// Get a live MCP client for a connector, connecting if needed.
    async fn get_or_connect(&self, connector: &Connector) -> Result<Arc<McpClient>> {
        if let Some(existing) = self.connections.get(&connector.id) {
            return Ok(Arc::clone(existing.value()));
        }
        let config: McpConfig = serde_json::from_value(connector.config.clone())
            .map_err(|e| anyhow!("Invalid MCP config: {}", e))?;
        config
            .validate()
            .map_err(|msg| anyhow!("Invalid MCP config: {}", msg))?;
        let client = Arc::new(connect_mcp(&config).await?);
        client.initialize().await?;
        self.connections
            .insert(connector.id.clone(), Arc::clone(&client));
        Ok(client)
    }
}

/// Establish a raw MCP connection (no initialize) from a config.
async fn connect_mcp(config: &McpConfig) -> Result<McpClient> {
    match config.transport.as_str() {
        "stdio" => {
            McpClient::connect_stdio(&config.command, &config.args, &config.env).await
        }
        "http" => Ok(McpClient::connect_http(&config.url, config.headers.clone())),
        other => Err(anyhow!("Unknown MCP transport: {}", other)),
    }
}

impl Connector {
    /// Tool-level enabled check (absent entries default to enabled).
    pub fn tool_enabled(&self, tool_name: &str) -> bool {
        self.tool_states.get(tool_name).copied().unwrap_or(true)
    }
}

impl Default for Connector {
    fn default() -> Self {
        Self {
            id: String::new(),
            user_id: String::new(),
            name: String::new(),
            ctype: String::new(),
            description: String::new(),
            config: serde_json::json!({}),
            tool_states: HashMap::new(),
            enabled: false,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }
}

/// A tool backed by a connector, registered in the dynamic registry area.
pub struct ConnectorTool {
    name: String,
    description: String,
    parameters: serde_json::Value,
    user_id: String,
    manager: Arc<ConnectorManager>,
}

#[async_trait]
impl Tool for ConnectorTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters(&self) -> serde_json::Value {
        self.parameters.clone()
    }

    async fn execute(&self, args: &str, ctx: &ToolContext) -> Result<String> {
        if ctx.user_id != self.user_id {
            bail!("Connector tool belongs to another user");
        }
        self.manager
            .execute_tool(&self.user_id, &self.name, args)
            .await
    }
}

/// Build LLM tool definitions from a set of connector tool infos
/// (helper for API responses and tests).
pub fn to_tool_definitions(
    connector_id: &str,
    tools: &[ConnectorToolInfo],
) -> Vec<ToolDefinition> {
    tools
        .iter()
        .map(|t| ToolDefinition {
            r#type: "function".to_string(),
            function: FunctionDefinition {
                name: global_tool_name(connector_id, &t.name),
                description: t.description.clone(),
                parameters: t.parameters.clone(),
            },
        })
        .collect()
}

/// Check whether a global tool name belongs to the connector subsystem.
pub fn is_connector_tool(name: &str) -> bool {
    parse_global_tool_name(name).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ApiToolDef;
    use serde_json::json;

    fn api_connector(id: &str, user_id: &str, tools: Vec<ApiToolDef>) -> Connector {
        Connector {
            id: id.to_string(),
            user_id: user_id.to_string(),
            name: "api-conn".to_string(),
            ctype: TYPE_API.to_string(),
            description: String::new(),
            config: serde_json::to_value(ApiConnectorConfig { tools }).unwrap(),
            tool_states: HashMap::new(),
            enabled: true,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    fn api_tool(name: &str, enabled: bool) -> ApiToolDef {
        ApiToolDef {
            name: name.to_string(),
            description: format!("{} description", name),
            method: "GET".to_string(),
            url: "https://example.com/x".to_string(),
            headers: HashMap::new(),
            params: json!({"type": "object"}),
            body_template: None,
            enabled,
        }
    }

    #[tokio::test]
    async fn test_sync_registry_respects_two_level_enablement() {
        let dir = tempfile::tempdir().unwrap();
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect(&format!(
                "sqlite:{}/sync.db?mode=rwc",
                dir.path().display()
            ))
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS connectors (
                id TEXT PRIMARY KEY, user_id TEXT NOT NULL, name TEXT NOT NULL,
                ctype TEXT NOT NULL, description TEXT NOT NULL DEFAULT '',
                config_json TEXT NOT NULL DEFAULT '{}',
                tool_states_json TEXT NOT NULL DEFAULT '{}',
                enabled INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now')))",
        )
        .execute(&pool)
        .await
        .unwrap();

        let manager = ConnectorManager::new(pool.clone());
        let mut registry = ToolRegistry::new();
        let registry = Arc::new(registry);
        // register_dynamic works on a shared registry
        manager.attach_registry(Arc::clone(&registry)).await;

        // One enabled tool, one disabled tool, one disabled connector
        manager
            .store()
            .create(&api_connector(
                "c1aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
                "u1",
                vec![api_tool("enabled_tool", true), api_tool("disabled_tool", false)],
            ))
            .await
            .unwrap();
        let mut c2 = api_connector(
            "c2aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
            "u1",
            vec![api_tool("offline_tool", true)],
        );
        c2.enabled = false;
        manager.store().create(&c2).await.unwrap();

        manager.sync_registry().await.unwrap();

        assert!(registry.has("connector_c1aaaaaa_enabled_tool"));
        assert!(!registry.has("connector_c1aaaaaa_disabled_tool"));
        assert!(!registry.has("connector_c2aaaaaa_offline_tool"));

        // Schemas of enabled tools appear in all_schemas
        let schemas = registry.all_schemas();
        assert!(schemas
            .iter()
            .any(|t| t.function.name == "connector_c1aaaaaa_enabled_tool"));

        // Disable the remaining tool -> sync removes it
        manager
            .store()
            .set_tool_enabled("u1", "c1aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee", "enabled_tool", false)
            .await
            .unwrap();
        manager.sync_registry().await.unwrap();
        assert!(!registry.has("connector_c1aaaaaa_enabled_tool"));

        // Executing an unregistered tool is rejected
        let err = manager
            .execute_tool("u1", "connector_c1aaaaaa_enabled_tool", "{}")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("disabled"));
    }

    #[tokio::test]
    async fn test_execute_tool_rejects_wrong_user() {
        let dir = tempfile::tempdir().unwrap();
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect(&format!(
                "sqlite:{}/exec.db?mode=rwc",
                dir.path().display()
            ))
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS connectors (
                id TEXT PRIMARY KEY, user_id TEXT NOT NULL, name TEXT NOT NULL,
                ctype TEXT NOT NULL, description TEXT NOT NULL DEFAULT '',
                config_json TEXT NOT NULL DEFAULT '{}',
                tool_states_json TEXT NOT NULL DEFAULT '{}',
                enabled INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now')))",
        )
        .execute(&pool)
        .await
        .unwrap();

        let manager = ConnectorManager::new(pool);
        manager
            .store()
            .create(&api_connector(
                "c9aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
                "u1",
                vec![api_tool("my_tool", true)],
            ))
            .await
            .unwrap();
        manager
            .attach_registry(Arc::new(ToolRegistry::new()))
            .await;
        manager.sync_registry().await.unwrap();

        // Wrong user cannot execute even if the tool is registered
        let err = manager
            .execute_tool("u2", "connector_c9aaaaaa_my_tool", "{}")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("another user"));
    }

    #[test]
    fn test_to_tool_definitions() {
        let infos = vec![ConnectorToolInfo {
            name: "echo".to_string(),
            description: "Echo".to_string(),
            parameters: json!({"type": "object"}),
            enabled: true,
        }];
        let defs = to_tool_definitions("abcd1234-0000", &infos);
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].function.name, "connector_abcd1234_echo");
        assert_eq!(defs[0].r#type, "function");
    }

    #[test]
    fn test_connector_tool_enabled_default() {
        let c = Connector::default();
        assert!(c.tool_enabled("anything")); // absent -> enabled
        let mut c = c;
        c.tool_states.insert("x".to_string(), false);
        assert!(!c.tool_enabled("x"));
    }

    #[test]
    fn test_is_connector_tool() {
        assert!(is_connector_tool("connector_abcd1234_foo"));
        assert!(!is_connector_tool("shell"));
        assert!(!is_connector_tool("connector_short_foo"));
    }
}
