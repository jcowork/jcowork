//! SQLite persistence for connectors.

use anyhow::{anyhow, Result};
use sqlx::SqlitePool;
use std::collections::HashMap;

use crate::models::{Connector, TYPE_API, TYPE_MCP};

/// Row tuple shape used by all queries.
type ConnectorRow = (
    String,         // id
    String,         // user_id
    String,         // name
    String,         // ctype
    String,         // description
    String,         // config_json
    String,         // tool_states_json
    bool,           // enabled
    String,         // created_at
    String,         // updated_at
);

fn row_to_connector(row: ConnectorRow) -> Connector {
    Connector {
        id: row.0,
        user_id: row.1,
        name: row.2,
        ctype: row.3,
        description: row.4,
        config: serde_json::from_str(&row.5).unwrap_or(serde_json::json!({})),
        tool_states: serde_json::from_str(&row.6).unwrap_or_default(),
        enabled: row.7,
        created_at: row.8,
        updated_at: row.9,
    }
}

/// SQLite-backed store for connectors (per-user rows in a shared table).
pub struct ConnectorStore {
    pool: SqlitePool,
}

impl ConnectorStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// List all connectors for a user, newest first.
    pub async fn list(&self, user_id: &str) -> Result<Vec<Connector>> {
        let rows: Vec<ConnectorRow> = sqlx::query_as(
            r#"
            SELECT id, user_id, name, ctype, description, config_json,
                   tool_states_json, enabled, created_at, updated_at
            FROM connectors
            WHERE user_id = ?
            ORDER BY created_at DESC
            "#,
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(row_to_connector).collect())
    }

    /// List only enabled connectors for a user.
    pub async fn list_enabled(&self, user_id: &str) -> Result<Vec<Connector>> {
        let rows: Vec<ConnectorRow> = sqlx::query_as(
            r#"
            SELECT id, user_id, name, ctype, description, config_json,
                   tool_states_json, enabled, created_at, updated_at
            FROM connectors
            WHERE user_id = ? AND enabled = 1
            ORDER BY created_at DESC
            "#,
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(row_to_connector).collect())
    }

    /// List every enabled connector across all users (used at startup to
    /// restore dynamic tool registrations).
    pub async fn list_all_enabled(&self) -> Result<Vec<Connector>> {
        let rows: Vec<ConnectorRow> = sqlx::query_as(
            r#"
            SELECT id, user_id, name, ctype, description, config_json,
                   tool_states_json, enabled, created_at, updated_at
            FROM connectors
            WHERE enabled = 1
            "#,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(row_to_connector).collect())
    }

    /// Get a connector by id, verifying ownership.
    pub async fn get(&self, user_id: &str, id: &str) -> Result<Connector> {
        let row: Option<ConnectorRow> = sqlx::query_as(
            r#"
            SELECT id, user_id, name, ctype, description, config_json,
                   tool_states_json, enabled, created_at, updated_at
            FROM connectors
            WHERE id = ? AND user_id = ?
            "#,
        )
        .bind(id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(row_to_connector)
            .ok_or_else(|| anyhow!("Connector not found: {}", id))
    }

    /// Insert a new connector.
    pub async fn create(&self, connector: &Connector) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO connectors
                (id, user_id, name, ctype, description, config_json, tool_states_json, enabled)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            "#,
        )
        .bind(&connector.id)
        .bind(&connector.user_id)
        .bind(&connector.name)
        .bind(&connector.ctype)
        .bind(&connector.description)
        .bind(connector.config.to_string())
        .bind(serde_json::to_string(&connector.tool_states)?)
        .bind(connector.enabled)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Update mutable fields of an existing connector (ownership-checked).
    pub async fn update(&self, connector: &Connector) -> Result<()> {
        let result = sqlx::query(
            r#"
            UPDATE connectors
            SET name = ?3, ctype = ?4, description = ?5, config_json = ?6,
                tool_states_json = ?7, enabled = ?8,
                updated_at = datetime('now')
            WHERE id = ?1 AND user_id = ?2
            "#,
        )
        .bind(&connector.id)
        .bind(&connector.user_id)
        .bind(&connector.name)
        .bind(&connector.ctype)
        .bind(&connector.description)
        .bind(connector.config.to_string())
        .bind(serde_json::to_string(&connector.tool_states)?)
        .bind(connector.enabled)
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 0 {
            return Err(anyhow!("Connector not found: {}", connector.id));
        }
        Ok(())
    }

    /// Delete a connector (ownership-checked).
    pub async fn delete(&self, user_id: &str, id: &str) -> Result<()> {
        let result = sqlx::query("DELETE FROM connectors WHERE id = ? AND user_id = ?")
            .bind(id)
            .bind(user_id)
            .execute(&self.pool)
            .await?;
        if result.rows_affected() == 0 {
            return Err(anyhow!("Connector not found: {}", id));
        }
        Ok(())
    }

    /// Toggle the connector-level enabled flag.
    pub async fn set_enabled(&self, user_id: &str, id: &str, enabled: bool) -> Result<()> {
        let result = sqlx::query(
            r#"
            UPDATE connectors
            SET enabled = ?3, updated_at = datetime('now')
            WHERE id = ?1 AND user_id = ?2
            "#,
        )
        .bind(id)
        .bind(user_id)
        .bind(enabled)
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 0 {
            return Err(anyhow!("Connector not found: {}", id));
        }
        Ok(())
    }

    /// Set the tool-level enabled flag for a single tool.
    ///
    /// - API connectors: updates `ApiToolDef.enabled` inside `config_json`.
    /// - MCP connectors: updates the `tool_states` JSON map.
    pub async fn set_tool_enabled(
        &self,
        user_id: &str,
        id: &str,
        tool_name: &str,
        enabled: bool,
    ) -> Result<()> {
        let connector = self.get(user_id, id).await?;
        match connector.ctype.as_str() {
            TYPE_API => {
                let mut cfg: crate::models::ApiConnectorConfig =
                    serde_json::from_value(connector.config.clone()).unwrap_or_default();
                let tool = cfg
                    .tools
                    .iter_mut()
                    .find(|t| t.name == tool_name)
                    .ok_or_else(|| anyhow!("Tool not found: {}", tool_name))?;
                tool.enabled = enabled;
                sqlx::query(
                    r#"
                    UPDATE connectors
                    SET config_json = ?3, updated_at = datetime('now')
                    WHERE id = ?1 AND user_id = ?2
                    "#,
                )
                .bind(id)
                .bind(user_id)
                .bind(serde_json::to_string(&cfg)?)
                .execute(&self.pool)
                .await?;
            }
            TYPE_MCP => {
                let mut states: HashMap<String, bool> = connector.tool_states.clone();
                states.insert(tool_name.to_string(), enabled);
                sqlx::query(
                    r#"
                    UPDATE connectors
                    SET tool_states_json = ?3, updated_at = datetime('now')
                    WHERE id = ?1 AND user_id = ?2
                    "#,
                )
                .bind(id)
                .bind(user_id)
                .bind(serde_json::to_string(&states)?)
                .execute(&self.pool)
                .await?;
            }
            other => return Err(anyhow!("Unknown connector type: {}", other)),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{ApiConnectorConfig, ApiToolDef, McpConfig};
    use serde_json::json;

    async fn setup() -> (tempfile::TempDir, ConnectorStore) {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect(&format!("sqlite:{}?mode=rwc", db_path.display()))
            .await
            .unwrap();
        // Minimal schema for unit tests (mirrors production migration)
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS connectors (
                id TEXT PRIMARY KEY,
                user_id TEXT NOT NULL,
                name TEXT NOT NULL,
                ctype TEXT NOT NULL,
                description TEXT NOT NULL DEFAULT '',
                config_json TEXT NOT NULL DEFAULT '{}',
                tool_states_json TEXT NOT NULL DEFAULT '{}',
                enabled INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            )
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();
        (dir, ConnectorStore::new(pool))
    }

    fn api_connector(id: &str, user_id: &str, tool_enabled: bool) -> Connector {
        let cfg = ApiConnectorConfig {
            tools: vec![ApiToolDef {
                name: "get_weather".to_string(),
                description: "Get weather".to_string(),
                method: "GET".to_string(),
                url: "https://api.example.com/weather?city={{city}}".to_string(),
                headers: Default::default(),
                params: json!({"type": "object"}),
                body_template: None,
                enabled: tool_enabled,
            }],
        };
        Connector {
            id: id.to_string(),
            user_id: user_id.to_string(),
            name: "weather".to_string(),
            ctype: TYPE_API.to_string(),
            description: "Weather API".to_string(),
            config: serde_json::to_value(&cfg).unwrap(),
            tool_states: Default::default(),
            enabled: true,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    fn mcp_connector(id: &str, user_id: &str) -> Connector {
        let cfg = McpConfig {
            transport: "stdio".to_string(),
            command: "npx".to_string(),
            args: vec!["-y".to_string(), "server".to_string()],
            env: Default::default(),
            url: String::new(),
            headers: Default::default(),
        };
        Connector {
            id: id.to_string(),
            user_id: user_id.to_string(),
            name: "mcp-server".to_string(),
            ctype: TYPE_MCP.to_string(),
            description: "MCP server".to_string(),
            config: serde_json::to_value(&cfg).unwrap(),
            tool_states: Default::default(),
            enabled: true,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[tokio::test]
    async fn test_create_get_delete() {
        let (_dir, store) = setup().await;
        let c = api_connector("c1", "u1", true);
        store.create(&c).await.unwrap();

        let fetched = store.get("u1", "c1").await.unwrap();
        assert_eq!(fetched.name, "weather");
        assert_eq!(fetched.ctype, TYPE_API);
        assert!(fetched.enabled);

        // Ownership check: other user cannot see it
        assert!(store.get("u2", "c1").await.is_err());

        store.delete("u1", "c1").await.unwrap();
        assert!(store.get("u1", "c1").await.is_err());
        // Deleting again fails
        assert!(store.delete("u1", "c1").await.is_err());
    }

    #[tokio::test]
    async fn test_list_and_enabled_filter() {
        let (_dir, store) = setup().await;
        store.create(&api_connector("c1", "u1", true)).await.unwrap();
        store.create(&api_connector("c2", "u1", true)).await.unwrap();
        store.create(&api_connector("c3", "u2", true)).await.unwrap();

        assert_eq!(store.list("u1").await.unwrap().len(), 2);
        assert_eq!(store.list("u2").await.unwrap().len(), 1);

        store.set_enabled("u1", "c2", false).await.unwrap();
        assert_eq!(store.list_enabled("u1").await.unwrap().len(), 1);
        assert_eq!(store.list_all_enabled().await.unwrap().len(), 2);

        // Toggling a non-existent connector fails
        assert!(store.set_enabled("u1", "nope", false).await.is_err());
    }

    #[tokio::test]
    async fn test_update() {
        let (_dir, store) = setup().await;
        let mut c = api_connector("c1", "u1", true);
        store.create(&c).await.unwrap();

        c.name = "weather-v2".to_string();
        c.description = "Updated".to_string();
        store.update(&c).await.unwrap();

        let fetched = store.get("u1", "c1").await.unwrap();
        assert_eq!(fetched.name, "weather-v2");
        assert_eq!(fetched.description, "Updated");

        // Cross-user update fails
        c.user_id = "u2".to_string();
        assert!(store.update(&c).await.is_err());
    }

    #[tokio::test]
    async fn test_set_tool_enabled_api_connector() {
        let (_dir, store) = setup().await;
        store.create(&api_connector("c1", "u1", true)).await.unwrap();

        store.set_tool_enabled("u1", "c1", "get_weather", false).await.unwrap();
        let c = store.get("u1", "c1").await.unwrap();
        let cfg: ApiConnectorConfig = serde_json::from_value(c.config).unwrap();
        assert!(!cfg.tools[0].enabled);

        store.set_tool_enabled("u1", "c1", "get_weather", true).await.unwrap();
        let c = store.get("u1", "c1").await.unwrap();
        let cfg: ApiConnectorConfig = serde_json::from_value(c.config).unwrap();
        assert!(cfg.tools[0].enabled);

        // Unknown tool name fails
        assert!(store.set_tool_enabled("u1", "c1", "missing", true).await.is_err());
    }

    #[tokio::test]
    async fn test_set_tool_enabled_mcp_connector() {
        let (_dir, store) = setup().await;
        store.create(&mcp_connector("m1", "u1")).await.unwrap();

        // MCP tools are auto-discovered: state goes into tool_states map.
        // Any tool name is accepted here (discovery happens elsewhere).
        store.set_tool_enabled("u1", "m1", "discovered_tool", false).await.unwrap();
        let c = store.get("u1", "m1").await.unwrap();
        assert_eq!(c.tool_states.get("discovered_tool"), Some(&false));

        store.set_tool_enabled("u1", "m1", "discovered_tool", true).await.unwrap();
        let c = store.get("u1", "m1").await.unwrap();
        assert_eq!(c.tool_states.get("discovered_tool"), Some(&true));
    }
}
