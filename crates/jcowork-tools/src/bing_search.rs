//! Web search tool using a headless Playwright browser.
//!
//! Invokes scripts/web_search.py via Python subprocess.
//! Primary: Sogou WAP interface (reliable for Chinese queries)
//! Fallback: cn.bing.com
//! Returns up to `num_results` structured results (title, url, snippet).

use anyhow::Result;
use async_trait::async_trait;
use tokio::process::Command;
use tokio::time::{timeout, Duration};

use crate::base::{Tool, ToolContext};

/// Resolve the Python binary path in the jcowork venv.
/// On Unix: ~/.jcowork/venv/bin/python
/// On Windows: ~/.jcowork/venv/Scripts/python.exe
fn resolve_python_bin() -> String {
    let base = shellexpand::tilde("~/.jcowork/venv").to_string();
    if cfg!(windows) {
        format!("{}\\Scripts\\python.exe", base)
    } else {
        format!("{}/bin/python", base)
    }
}

/// Path to the search script (relative to workspace root or absolute).
const SEARCH_SCRIPT: &str = "scripts/web_search.py";

/// Web search tool — uses Sogou WAP (primary) / cn.bing.com (fallback) via headless Chromium.
/// Returns titles, URLs, and snippets as structured text.
pub struct WebSearchTool {
    /// Absolute path to the workspace / project root (to locate the script).
    pub workspace_root: String,
}

impl Default for WebSearchTool {
    fn default() -> Self {
        // Resolve the script path relative to the binary location at runtime
        let root = std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        Self { workspace_root: root }
    }
}

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str { "web_search" }

    fn description(&self) -> &str {
        "Search the web using a headless browser (Sogou). \
         Returns up to 20 real search results (title, URL, snippet). \
         Use this whenever the question requires up-to-date information from the internet."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query string"
                },
                "num_results": {
                    "type": "integer",
                    "description": "Number of results to return (default: 20, max: 20)",
                    "default": 20
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(&self, args: &str, _ctx: &ToolContext) -> Result<String> {
        let params: serde_json::Value = serde_json::from_str(args)?;
        let query = params["query"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'query' parameter"))?;
        let num_results = params["num_results"].as_u64().unwrap_or(20).min(20);

        let python_bin = resolve_python_bin();

        // Resolve script path: try absolute first, then relative to workspace root
        let script_path = {
            let abs = std::path::Path::new(&self.workspace_root).join(SEARCH_SCRIPT);
            if abs.exists() {
                abs.to_string_lossy().to_string()
            } else {
                // Fallback: try next to the binary
                let exe_dir = std::env::current_exe()
                    .ok()
                    .and_then(|p| p.parent().map(|d| d.to_string_lossy().to_string()))
                    .unwrap_or_default();
                std::path::Path::new(&exe_dir).join(SEARCH_SCRIPT).to_string_lossy().to_string()
            }
        };

        let result = timeout(
            Duration::from_secs(60),
            Command::new(&python_bin)
                .arg(&script_path)
                .arg(query)
                .arg(num_results.to_string())
                .output(),
        )
        .await;

        let output = match result {
            Ok(Ok(o)) => o,
            Ok(Err(e)) => return Ok(format!("web_search: failed to spawn process: {}", e)),
            Err(_) => return Ok("web_search: timed out after 60s".to_string()),
        };

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Ok(format!("web_search error: {}", stderr.trim()));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let parsed: serde_json::Value = match serde_json::from_str(&stdout) {
            Ok(v) => v,
            Err(_) => return Ok(format!("web_search: unexpected output: {}", stdout.trim())),
        };

        // Check for error object returned by the script
        if let Some(err) = parsed.get("error").and_then(|e| e.as_str()) {
            return Ok(format!("web_search error: {}", err));
        }

        // Format results as readable text
        let results = parsed.as_array().ok_or_else(|| anyhow::anyhow!("Expected JSON array"))?;
        if results.is_empty() {
            return Ok("No search results found.".to_string());
        }

        let lines: Vec<String> = results
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let title = r["title"].as_str().unwrap_or("");
                let url = r["url"].as_str().unwrap_or("");
                let snippet = r["snippet"].as_str().unwrap_or("");
                format!("{}. {}\n   URL: {}\n   {}", i + 1, title, url, snippet)
            })
            .collect();

        Ok(format!(
            "Web search results for \"{}\" ({} results):\n\n{}",
            query,
            results.len(),
            lines.join("\n\n")
        ))
    }
}
