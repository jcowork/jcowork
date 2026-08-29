//! Data models for connectors.
//!
//! A connector is an external capability source owned by a user. Two types
//! are supported:
//! - `api`: a set of manually defined HTTP tools (name/description/method/URL/...)
//! - `mcp`: a Model Context Protocol server (stdio or HTTP transport) whose
//!   tools are discovered via `tools/list`.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Connector type constant: HTTP API tools.
pub const TYPE_API: &str = "api";
/// Connector type constant: MCP server.
pub const TYPE_MCP: &str = "mcp";

/// A connector row as persisted in the `connectors` table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Connector {
    pub id: String,
    pub user_id: String,
    pub name: String,
    /// Connector type: "api" or "mcp".
    pub ctype: String,
    pub description: String,
    /// Parsed connector configuration ([`ApiConnectorConfig`] or [`McpConfig`]).
    pub config: serde_json::Value,
    /// Per-tool enabled state for MCP connectors: tool_name -> enabled.
    /// Absent entries are treated as enabled. Unused by API connectors
    /// (their state lives in [`ApiToolDef::enabled`]).
    pub tool_states: HashMap<String, bool>,
    /// Connector-level enabled flag.
    pub enabled: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// Configuration for an API connector: a list of manually defined HTTP tools.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ApiConnectorConfig {
    #[serde(default)]
    pub tools: Vec<ApiToolDef>,
}

/// A single HTTP tool defined inside an API connector.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiToolDef {
    pub name: String,
    /// Description shown to the LLM for tool selection.
    pub description: String,
    /// HTTP method (GET/POST/PUT/PATCH/DELETE). Defaults to GET.
    #[serde(default = "default_method")]
    pub method: String,
    /// URL template; may contain `{{param}}` placeholders rendered from args.
    pub url: String,
    /// Literal request headers (e.g. Authorization).
    #[serde(default)]
    pub headers: HashMap<String, String>,
    /// JSON Schema of the tool parameters (presented to the LLM).
    #[serde(default)]
    pub params: serde_json::Value,
    /// Optional body template with `{{param}}` placeholders. When absent,
    /// the full args object is serialized as the JSON body for
    /// POST/PUT/PATCH requests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_template: Option<String>,
    /// Per-tool enabled state (defaults to true).
    #[serde(default = "default_true")]
    pub enabled: bool,
}

/// Configuration for an MCP connector.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpConfig {
    /// Transport: "stdio" or "http".
    pub transport: String,
    /// stdio: command to launch (e.g. "npx", "uvx", "python").
    #[serde(default)]
    pub command: String,
    /// stdio: command arguments.
    #[serde(default)]
    pub args: Vec<String>,
    /// stdio: extra environment variables for the child process.
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// http: MCP server endpoint URL.
    #[serde(default)]
    pub url: String,
    /// http: extra request headers (e.g. Authorization).
    #[serde(default)]
    pub headers: HashMap<String, String>,
}

impl McpConfig {
    /// Validate the config has enough fields for its transport.
    pub fn validate(&self) -> Result<(), String> {
        match self.transport.as_str() {
            "stdio" => {
                if self.command.trim().is_empty() {
                    return Err("stdio transport requires a non-empty command".to_string());
                }
                Ok(())
            }
            "http" => {
                if !self.url.starts_with("http://") && !self.url.starts_with("https://") {
                    return Err("http transport requires a valid http(s) url".to_string());
                }
                Ok(())
            }
            other => Err(format!("unknown MCP transport: {}", other)),
        }
    }
}

/// A tool inside a connector, as reported to the frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectorToolInfo {
    /// Original tool name within the connector.
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
    /// Effective enabled state (tool-level).
    pub enabled: bool,
}

fn default_method() -> String {
    "GET".to_string()
}

fn default_true() -> bool {
    true
}

/// Sanitize a tool name so it satisfies LLM function-name rules
/// (only `[a-zA-Z0-9_-]` are allowed).
pub fn sanitize_tool_name(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .collect();
    if s.is_empty() {
        "tool".to_string()
    } else {
        s
    }
}

/// Build the globally unique registry name for a connector tool.
///
/// Format: `connector_<8-char connector id prefix>_<sanitized tool name>`.
/// The id prefix guarantees uniqueness across connectors and users.
pub fn global_tool_name(connector_id: &str, tool_name: &str) -> String {
    let prefix: String = connector_id.chars().take(8).collect();
    format!("connector_{}_{}", prefix, sanitize_tool_name(tool_name))
}

/// Extract the connector id prefix embedded in a global tool name.
///
/// Returns None when the name does not look like a connector tool.
pub fn parse_global_tool_name(global_name: &str) -> Option<(&str, &str)> {
    let rest = global_name.strip_prefix("connector_")?;
    let (prefix, tool) = rest.split_once('_')?;
    if prefix.len() != 8 {
        return None;
    }
    Some((prefix, tool))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_tool_name() {
        assert_eq!(sanitize_tool_name("get_weather"), "get_weather");
        assert_eq!(sanitize_tool_name("search.docs"), "search_docs");
        assert_eq!(sanitize_tool_name("搜索"), "__");
        assert_eq!(sanitize_tool_name(""), "tool");
        assert_eq!(sanitize_tool_name("a-b_c9"), "a-b_c9");
    }

    #[test]
    fn test_global_tool_name_roundtrip() {
        let id = "01234567-89ab-cdef-0123-456789abcdef";
        let global = global_tool_name(id, "get_weather");
        assert_eq!(global, "connector_01234567_get_weather");
        let (prefix, tool) = parse_global_tool_name(&global).unwrap();
        assert_eq!(prefix, "01234567");
        assert_eq!(tool, "get_weather");
    }

    #[test]
    fn test_parse_global_tool_name_rejects_foreign_names() {
        assert!(parse_global_tool_name("shell").is_none());
        assert!(parse_global_tool_name("connector_short_x").is_none());
        assert!(parse_global_tool_name("connector_").is_none());
    }

    #[test]
    fn test_api_tool_def_defaults() {
        let def: ApiToolDef = serde_json::from_value(serde_json::json!({
            "name": "t",
            "description": "d",
            "url": "http://x"
        }))
        .unwrap();
        assert_eq!(def.method, "GET");
        assert!(def.enabled);
        assert!(def.headers.is_empty());
    }

    #[test]
    fn test_mcp_config_validate() {
        let stdio_ok: McpConfig = serde_json::from_value(serde_json::json!({
            "transport": "stdio", "command": "npx", "args": ["-y", "server"]
        }))
        .unwrap();
        assert!(stdio_ok.validate().is_ok());

        let stdio_bad: McpConfig = serde_json::from_value(serde_json::json!({
            "transport": "stdio", "command": ""
        }))
        .unwrap();
        assert!(stdio_bad.validate().is_err());

        let http_bad: McpConfig = serde_json::from_value(serde_json::json!({
            "transport": "http", "url": "ftp://x"
        }))
        .unwrap();
        assert!(http_bad.validate().is_err());

        let unknown: McpConfig = serde_json::from_value(serde_json::json!({
            "transport": "carrier-pigeon"
        }))
        .unwrap();
        assert!(unknown.validate().is_err());
    }
}
