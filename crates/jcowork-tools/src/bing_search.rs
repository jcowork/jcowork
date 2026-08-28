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
use std::sync::Arc;

use crate::base::{Tool, ToolContext, truncate_str};
use jcowork_logs::{LogEntry, LogWriter};

/// Resolve the home directory. On Windows the desktop app process may not
/// have HOME set, so fall back to USERPROFILE (same convention as
/// jcowork-server::config and jcowork-storage::docling_manager).
fn home_dir() -> String {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_default()
}

/// Resolve the Python binary path in the jcowork venv.
/// On Unix: ~/.jcowork/venv/bin/python
/// On Windows: ~/.jcowork/venv/Scripts/python.exe
/// Falls back to system Python (`python3`/`python`) if the venv is missing,
/// so the tool degrades gracefully instead of failing with OS error 3.
fn resolve_python_bin() -> String {
    let venv_bin = if cfg!(windows) {
        format!("{}\\.jcowork\\venv\\Scripts\\python.exe", home_dir())
    } else {
        format!("{}/.jcowork/venv/bin/python", home_dir())
    };
    if std::path::Path::new(&venv_bin).exists() {
        return venv_bin;
    }
    // Venv not set up yet — try system Python from PATH
    for candidate in ["python3", "python"] {
        if which(candidate).is_some() {
            return candidate.to_string();
        }
    }
    venv_bin
}

/// Minimal `which` implementation using the PATH environment variable.
fn which(cmd: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    let exe_names: Vec<String> = if cfg!(windows) {
        vec![format!("{}.exe", cmd), cmd.to_string()]
    } else {
        vec![cmd.to_string()]
    };
    for dir in std::env::split_paths(&path) {
        for name in &exe_names {
            let full = dir.join(name);
            if full.is_file() {
                return Some(full);
            }
        }
    }
    None
}

/// Extract a meaningful error message from a failed subprocess run.
/// The script reports errors as JSON `{"error": "..."}` on stdout;
/// only fall back to stderr if stdout has no structured error
/// (otherwise the LLM receives an empty, useless error message).
fn extract_subprocess_error(stdout: &[u8], stderr: &[u8]) -> String {
    let stdout_str = String::from_utf8_lossy(stdout);
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&stdout_str) {
        if let Some(err) = parsed.get("error").and_then(|e| e.as_str()) {
            if !err.is_empty() {
                return err.to_string();
            }
        }
    }
    String::from_utf8_lossy(stderr).trim().to_string()
}

/// Path to the search script (relative to workspace root or absolute).
const SEARCH_SCRIPT: &str = "scripts/web_search.py";

/// Locate web_search.py. Search order (first existing wins):
///   1. JCWORK_SCRIPTS_DIR env var (set by the desktop app from bundle resources)
///   2. workspace_root/scripts/web_search.py (cwd = project root)
///   3. next to the executable, or its scripts/ subdir
///   4. exe_dir/../../scripts/ (dev build: target/release → project root)
fn resolve_script_path(workspace_root: &str) -> String {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()));

    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(scripts_dir) = std::env::var("JCWORK_SCRIPTS_DIR") {
        candidates.push(std::path::Path::new(&scripts_dir).join("web_search.py"));
    }
    candidates.push(std::path::Path::new(workspace_root).join(SEARCH_SCRIPT));
    if let Some(ref dir) = exe_dir {
        candidates.push(dir.join("web_search.py"));
        candidates.push(dir.join(SEARCH_SCRIPT));
        if let Some(grand) = dir.parent().and_then(|p| p.parent()) {
            candidates.push(grand.join(SEARCH_SCRIPT));
        }
    }
    for c in &candidates {
        if c.exists() {
            return c.to_string_lossy().to_string();
        }
    }
    // Nothing found — return the first candidate so the error is understandable
    candidates
        .first()
        .map(|c| c.to_string_lossy().to_string())
        .unwrap_or_else(|| SEARCH_SCRIPT.to_string())
}

/// Web search tool — uses Sogou WAP (primary) / cn.bing.com (fallback) via headless Chromium.
/// Returns titles, URLs, and snippets as structured text.
pub struct WebSearchTool {
    /// Absolute path to the workspace / project root (to locate the script).
    pub workspace_root: String,
    /// Optional log writer for recording search results.
    pub log_writer: Option<Arc<LogWriter>>,
}

impl Default for WebSearchTool {
    fn default() -> Self {
        // Resolve the script path relative to the binary location at runtime
        let root = std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        Self { 
            workspace_root: root,
            log_writer: None,
        }
    }
}

impl WebSearchTool {
    /// Set the log writer for recording search results.
    pub fn with_log_writer(mut self, log_writer: Arc<LogWriter>) -> Self {
        self.log_writer = Some(log_writer);
        self
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

        // Resolve script path across all known locations (env var, workspace,
        // executable dir, dev-build project root)
        let script_path = resolve_script_path(&self.workspace_root);

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
            let err = extract_subprocess_error(&output.stdout, &output.stderr);
            return Ok(format!("web_search error: {}", err));
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

        // Log raw search results (limit size for performance)
        let log_results: Vec<serde_json::Value> = results
            .iter()
            .map(|r| {
                serde_json::json!({
                    "title": r["title"].as_str().unwrap_or(""),
                    "url": r["url"].as_str().unwrap_or(""),
                    "snippet": r["snippet"].as_str().unwrap_or(""),
                    "content": r["content"].as_str().map(|c| {
                        if c.len() > 500 { format!("{}...(truncated)", truncate_str(c, 500)) } else { c.to_string() }
                    }).unwrap_or_default(),
                })
            })
            .collect();
        
        if let Some(ref log_writer) = self.log_writer {
            let log_entry = LogEntry::RawData {
                timestamp: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                source: "web_search".to_string(),
                data: serde_json::json!({
                    "query": query,
                    "num_results": results.len(),
                    "results": log_results,
                }),
            };
            let lw = log_writer.clone();
            tokio::spawn(async move { 
                let _ = lw.write(&log_entry).await; 
            });
        }

        // Format results for LLM (limit to first 10 results to avoid token overflow)
        let lines: Vec<String> = results
            .iter()
            .take(10)  // Only take first 10 results
            .enumerate()
            .map(|(i, r)| {
                let title = r["title"].as_str().unwrap_or("");
                let url = r["url"].as_str().unwrap_or("");
                let snippet = r["snippet"].as_str().unwrap_or("");
                let content = r["content"].as_str().unwrap_or("");
                
                let mut result_text = format!("{}. {}\n   URL: {}\n   Snippet: {}", i + 1, title, url, snippet);
                
                // Add detailed content if available (for top 3 results only)
                if i < 3 && !content.is_empty() && content != "(No content extracted)" && !content.starts_with("(Failed") {
                    let content_preview = if content.len() > 500 {
                        format!("{}... (truncated)", truncate_str(content, 500))
                    } else {
                        content.to_string()
                    };
                    result_text.push_str(&format!("\n   Content: {}", content_preview));
                }
                
                result_text
            })
            .collect();

        let result_text = format!(
            "Web search results for \"{}\" ({} results, showing top 10, first 3 with full content):\n\n{}",
            query,
            results.len(),
            lines.join("\n\n")
        );
        
        // Ensure result is not too large (max ~15KB)
        let result_text = if result_text.len() > 15000 {
            format!("{}...(result truncated due to size)", &result_text[..15000])
        } else {
            result_text
        };
        
        Ok(result_text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    /// Create `dir/scripts/web_search.py` so the path exists as a candidate.
    fn create_script(dir: &std::path::Path) -> std::path::PathBuf {
        let scripts = dir.join("scripts");
        std::fs::create_dir_all(&scripts).unwrap();
        let script = scripts.join("web_search.py");
        std::fs::File::create(&script).unwrap().write_all(b"# test").unwrap();
        script
    }

    // ── Script path resolution ──────────────────────────────────────────

    /// All JCWORK_SCRIPTS_DIR-dependent cases run in one test to avoid
    /// env-var races with parallel tests.
    #[test]
    fn test_resolve_script_path_candidates() {
        // 1. JCWORK_SCRIPTS_DIR points to a dir containing web_search.py → wins
        let env_dir = tempdir().unwrap();
        let env_script = env_dir.path().join("web_search.py");
        std::fs::File::create(&env_script).unwrap().write_all(b"# test").unwrap();
        unsafe { std::env::set_var("JCWORK_SCRIPTS_DIR", env_dir.path()); }
        let got = resolve_script_path("/nonexistent-workspace");
        assert_eq!(
            std::path::Path::new(&got).canonicalize().unwrap(),
            env_script.canonicalize().unwrap(),
            "env var candidate should win when the script exists there"
        );

        // 2. Env var removed → workspace_root/scripts/web_search.py is used
        unsafe { std::env::remove_var("JCWORK_SCRIPTS_DIR"); }
        let ws = tempdir().unwrap();
        let ws_script = create_script(ws.path());
        let got = resolve_script_path(&ws.path().to_string_lossy());
        assert_eq!(
            std::path::Path::new(&got).canonicalize().unwrap(),
            ws_script.canonicalize().unwrap(),
            "workspace candidate should be used when env var is unset"
        );

        // 3. No candidate in env/workspace → still returns a path (no panic).
        //    In dev builds the exe-dir grandparent (project scripts/) may be
        //    found, so only assert a non-empty result here.
        let empty_ws = tempdir().unwrap();
        let got = resolve_script_path(&empty_ws.path().to_string_lossy());
        assert!(!got.is_empty(), "must always return a candidate path");
    }

    #[test]
    fn test_resolve_script_path_env_var_without_script_falls_through() {
        // JCWORK_SCRIPTS_DIR set but no script there → must fall through to
        // the workspace candidate instead of returning the missing env path.
        let bogus_dir = tempdir().unwrap();
        unsafe { std::env::set_var("JCWORK_SCRIPTS_DIR", bogus_dir.path()); }
        let ws = tempdir().unwrap();
        let ws_script = create_script(ws.path());
        let got = resolve_script_path(&ws.path().to_string_lossy());
        unsafe { std::env::remove_var("JCWORK_SCRIPTS_DIR"); }
        assert_eq!(
            std::path::Path::new(&got).canonicalize().unwrap(),
            ws_script.canonicalize().unwrap(),
            "missing env-var script must fall through to workspace candidate"
        );
    }

    // ── Subprocess error extraction ─────────────────────────────────────

    #[test]
    fn test_extract_subprocess_error_json_on_stdout() {
        let stdout = br#"{"error": "playwright not installed"}"#;
        let err = extract_subprocess_error(stdout, b"ignored stderr");
        assert_eq!(err, "playwright not installed");
    }

    #[test]
    fn test_extract_subprocess_error_plain_stderr() {
        // Non-JSON stdout (e.g. Python "can't open file") → use trimmed stderr
        let stdout = b"";
        let stderr = b"python.exe: can't open file 'x.py'\r\n";
        let err = extract_subprocess_error(stdout, stderr);
        assert_eq!(err, "python.exe: can't open file 'x.py'");
    }

    #[test]
    fn test_extract_subprocess_error_json_without_error_field() {
        let stdout = br#"{"results": []}"#;
        let err = extract_subprocess_error(stdout, b"some stderr");
        assert_eq!(err, "some stderr");
    }

    #[test]
    fn test_extract_subprocess_error_empty_error_field_falls_back() {
        // Empty error string (the previously swallowed case) → stderr instead
        let stdout = br#"{"error": ""}"#;
        let err = extract_subprocess_error(stdout, b"real failure reason");
        assert_eq!(err, "real failure reason");
    }

    #[test]
    fn test_extract_subprocess_error_both_empty() {
        let err = extract_subprocess_error(b"", b"");
        assert_eq!(err, "");
    }

    // ── Home dir / python resolution ────────────────────────────────────

    #[test]
    fn test_resolve_python_bin_points_into_jcowork_venv() {
        // Must never contain an unexpanded tilde (the old shellexpand bug
        // when HOME is unset in the desktop app process).
        let bin = resolve_python_bin();
        assert!(!bin.contains('~'), "unexpanded tilde in python path: {}", bin);
        assert!(
            bin.contains(".jcowork") || bin == "python3" || bin == "python",
            "unexpected python bin: {}",
            bin
        );
    }

    #[test]
    fn test_home_dir_prefers_home_then_userprofile() {
        // Only assert consistency with the environment, not specific values
        // (mutating HOME/USERPROFILE here would race parallel tests).
        let home = home_dir();
        let expected = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_default();
        assert_eq!(home, expected);
    }

    #[test]
    fn test_which_returns_none_for_missing_command() {
        assert!(which("definitely-not-a-real-cmd-xyz-12345").is_none());
    }
}
