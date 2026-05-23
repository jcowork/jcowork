//! Web search tool.

use anyhow::Result;
use async_trait::async_trait;

use crate::base::{Tool, ToolContext};

/// Web search tool using SearXNG or a similar API.
pub struct WebSearchTool {
    searxng_url: String,
}

impl WebSearchTool {
    pub fn new(searxng_url: String) -> Self {
        Self { searxng_url }
    }
}

impl Default for WebSearchTool {
    fn default() -> Self {
        Self::new("http://localhost:8888".to_string())
    }
}

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str { "web_search" }
    fn description(&self) -> &str { "Search the web using SearXNG. Returns top results with titles, URLs, and snippets." }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Search query" },
                "num_results": { "type": "integer", "description": "Number of results (default: 5)", "default": 5 }
            },
            "required": ["query"]
        })
    }
    async fn execute(&self, args: &str, _ctx: &ToolContext) -> Result<String> {
        let parsed: serde_json::Value = serde_json::from_str(args)?;
        let query = parsed["query"].as_str().ok_or_else(|| anyhow::anyhow!("Missing 'query'"))?;
        let num = parsed["num_results"].as_u64().unwrap_or(5) as usize;

        let url = format!("{}/search?q={}&format=json&limit={}", self.searxng_url, query, num);
        let client = reqwest::Client::new();
        let resp = client.get(&url).send().await;

        match resp {
            Ok(resp) if resp.status().is_success() => {
                let data: serde_json::Value = resp.json().await?;
                let results = data["results"]
                    .as_array()
                    .map(|arr| {
                        arr.iter().take(num).enumerate().map(|(i, r)| {
                            format!(
                                "{}. {} - {}\n   {}",
                                i + 1,
                                r["title"].as_str().unwrap_or(""),
                                r["url"].as_str().unwrap_or(""),
                                r["content"].as_str().unwrap_or("")
                            )
                        }).collect::<Vec<_>>().join("\n\n")
                    })
                    .unwrap_or_else(|| "No results found".to_string());
                Ok(results)
            }
            Ok(resp) => Ok(format!("Search failed: HTTP {}", resp.status())),
            Err(e) => Ok(format!("Search error: {}. Ensure SearXNG is running at {}", e, self.searxng_url)),
        }
    }
}
