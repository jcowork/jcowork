//! Lightweight MCP (Model Context Protocol) client.
//!
//! Implements JSON-RPC 2.0 over two transports:
//! - stdio: launches a local child process and exchanges newline-delimited
//!   JSON messages over stdin/stdout.
//! - http: POSTs JSON-RPC messages to a streamable HTTP endpoint, accepting
//!   either `application/json` or single-event `text/event-stream` replies.
//!
//! Supported protocol methods: `initialize`, `notifications/initialized`,
//! `tools/list`, `tools/call`.

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;

/// Timeout for a single JSON-RPC request.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

const PROTOCOL_VERSION: &str = "2025-03-26";

/// A tool schema discovered from an MCP server.
#[derive(Debug, Clone)]
pub struct McpTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

type PendingMap = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value>>>>>;

enum Transport {
    Stdio {
        writer: mpsc::UnboundedSender<String>,
        pending: PendingMap,
        child: Mutex<Option<Child>>,
    },
    Http(HttpTransport),
}

struct HttpTransport {
    client: reqwest::Client,
    url: String,
    headers: HashMap<String, String>,
    session_id: Mutex<Option<String>>,
}

/// An MCP client connection (one per connector instance).
pub struct McpClient {
    transport: Transport,
    next_id: AtomicU64,
}

impl McpClient {
    /// Connect to an MCP server over stdio by launching a child process.
    pub async fn connect_stdio(
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
    ) -> Result<Self> {
        let mut child = spawn_stdio_process(command, args, env).await?;

        let stdin = child.stdin.take().ok_or_else(|| anyhow!("Failed to capture stdin"))?;
        let stdout = child.stdout.take().ok_or_else(|| anyhow!("Failed to capture stdout"))?;

        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));

        // Writer task: forward queued JSON lines to the child's stdin.
        let (writer_tx, mut writer_rx) = mpsc::unbounded_channel::<String>();
        tokio::spawn(async move {
            let mut stdin = stdin;
            while let Some(line) = writer_rx.recv().await {
                if stdin.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
                if stdin.write_all(b"\n").await.is_err() {
                    break;
                }
                let _ = stdin.flush().await;
            }
        });

        // Reader task: parse JSON-RPC responses and resolve pending requests.
        let reader_pending = Arc::clone(&pending);
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let line = line.trim().to_string();
                if line.is_empty() {
                    continue;
                }
                let msg: Value = match serde_json::from_str(&line) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                // Only handle responses (id present with result or error).
                let id = match msg.get("id").and_then(|v| v.as_u64()) {
                    Some(id) => id,
                    None => continue, // notification or server request — ignored
                };
                let outcome = if let Some(err) = msg.get("error") {
                    let message = err
                        .get("message")
                        .and_then(|m| m.as_str())
                        .unwrap_or("unknown MCP error");
                    Err(anyhow!("MCP error: {}", message))
                } else {
                    Ok(msg.get("result").cloned().unwrap_or(Value::Null))
                };
                if let Some(tx) = reader_pending.lock().unwrap().remove(&id) {
                    let _ = tx.send(outcome);
                }
            }
            // Connection closed: fail all pending requests.
            let mut map = reader_pending.lock().unwrap();
            for (_, tx) in map.drain() {
                let _ = tx.send(Err(anyhow!("MCP stdio connection closed")));
            }
        });

        Ok(Self {
            transport: Transport::Stdio {
                writer: writer_tx,
                pending,
                child: Mutex::new(Some(child)),
            },
            next_id: AtomicU64::new(1),
        })
    }

    /// Connect to an MCP server over streamable HTTP.
    ///
    /// The client is built with `no_proxy`: MCP endpoints are typically
    /// localhost or intranet services, and system proxies frequently
    /// interfere with them.
    pub fn connect_http(url: &str, headers: HashMap<String, String>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .no_proxy()
            .build()
            .unwrap_or_default();
        Self {
            transport: Transport::Http(HttpTransport {
                client,
                url: url.to_string(),
                headers,
                session_id: Mutex::new(None),
            }),
            next_id: AtomicU64::new(1),
        }
    }

    /// Perform the MCP handshake. Must be called before any other request.
    pub async fn initialize(&self) -> Result<()> {
        let params = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {
                "name": "jcowork",
                "version": env!("CARGO_PKG_VERSION"),
            }
        });
        let _result = self.request("initialize", params).await?;
        // Servers may negotiate an older protocolVersion; we accept whatever
        // they report and proceed.
        self.notify("notifications/initialized", json!({})).await?;
        Ok(())
    }

    /// List the tools exposed by the MCP server.
    pub async fn list_tools(&self) -> Result<Vec<McpTool>> {
        let result = self.request("tools/list", json!({})).await?;
        let tools = result
            .get("tools")
            .and_then(|t| t.as_array())
            .cloned()
            .unwrap_or_default();
        Ok(tools
            .into_iter()
            .filter_map(|t| {
                let name = t.get("name")?.as_str()?.to_string();
                let description = t
                    .get("description")
                    .and_then(|d| d.as_str())
                    .unwrap_or("")
                    .to_string();
                let input_schema = t
                    .get("inputSchema")
                    .cloned()
                    .unwrap_or(json!({"type": "object"}));
                Some(McpTool {
                    name,
                    description,
                    input_schema,
                })
            })
            .collect())
    }

    /// Call a tool on the MCP server and return its text content.
    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<String> {
        let result = self
            .request("tools/call", json!({ "name": name, "arguments": arguments }))
            .await?;
        if result.get("isError").and_then(|v| v.as_bool()) == Some(true) {
            return Err(anyhow!("MCP tool '{}' failed: {}", name, extract_text(&result)));
        }
        let text = extract_text(&result);
        if text.is_empty() {
            // Fall back to the raw result so the model still gets something.
            return Ok(serde_json::to_string(&result).unwrap_or_default());
        }
        Ok(text)
    }

    /// Send a JSON-RPC request and wait for its response result.
    async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let msg = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        match &self.transport {
            Transport::Stdio { writer, pending, .. } => {
                let (tx, rx) = oneshot::channel();
                pending.lock().unwrap().insert(id, tx);
                writer
                    .send(serde_json::to_string(&msg)?)
                    .map_err(|_| anyhow!("MCP stdio writer closed"))?;
                let outcome = timeout(REQUEST_TIMEOUT, rx)
                    .await
                    .map_err(|_| {
                        pending.lock().unwrap().remove(&id);
                        anyhow!("MCP request '{}' timed out", method)
                    })?
                    .map_err(|_| anyhow!("MCP response channel closed"))??;
                Ok(outcome)
            }
            Transport::Http(http) => {
                let result = timeout(REQUEST_TIMEOUT, http.post(&msg, true))
                    .await
                    .map_err(|_| anyhow!("MCP request '{}' timed out", method))??;
                extract_rpc_result(id, &result)
            }
        }
    }

    /// Send a JSON-RPC notification (no response expected).
    async fn notify(&self, method: &str, params: Value) -> Result<()> {
        let msg = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        match &self.transport {
            Transport::Stdio { writer, .. } => {
                writer
                    .send(serde_json::to_string(&msg)?)
                    .map_err(|_| anyhow!("MCP stdio writer closed"))?;
                Ok(())
            }
            Transport::Http(http) => {
                // Notifications may return 202 with no body; ignore errors
                // only for transport-level issues on accepted status codes.
                let _ = http.post(&msg, false).await;
                Ok(())
            }
        }
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        if let Transport::Stdio { child, .. } = &self.transport {
            // Take the child out and let it drop here; the process was
            // spawned with kill_on_drop(true) so it is terminated.
            let _ = child.lock().unwrap().take();
        }
    }
}

/// Launch a stdio MCP server process.
///
/// On Windows, bare commands like `npx` resolve to `npx.cmd`, which
/// `CreateProcessW` cannot find; fall back to launching through `cmd /C`.
async fn spawn_stdio_process(
    command: &str,
    args: &[String],
    env: &HashMap<String, String>,
) -> Result<Child> {
    let build = |cmd_path: &str, cmd_args: Vec<String>| {
        let mut cmd = Command::new(cmd_path);
        cmd.args(cmd_args)
            .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        cmd
    };

    match build(command, args.to_vec()).spawn() {
        Ok(child) => Ok(child),
        Err(e) => {
            if cfg!(windows) && !std::path::Path::new(command).extension().is_some_and(|x| !x.is_empty()) {
                let mut wrapped = vec!["/C".to_string(), command.to_string()];
                wrapped.extend(args.iter().cloned());
                return build("cmd.exe", wrapped)
                    .spawn()
                    .map_err(|e2| anyhow!("Failed to launch '{}': {} (cmd /c retry: {})", command, e, e2));
            }
            Err(anyhow!("Failed to launch '{}': {}", command, e))
        }
    }
}

impl HttpTransport {
    /// POST a JSON-RPC message and parse the reply (JSON or single-event SSE).
    async fn post(&self, msg: &Value, expect_response: bool) -> Result<Value> {
        let mut req = self
            .client
            .post(&self.url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .body(serde_json::to_string(msg)?);
        for (k, v) in &self.headers {
            req = req.header(k.as_str(), v.as_str());
        }
        if let Some(session) = self.session_id.lock().unwrap().clone() {
            req = req.header("Mcp-Session-Id", session);
        }

        let resp = req.send().await.map_err(|e| anyhow!("MCP HTTP request failed: {}", e))?;
        let status = resp.status();

        // Capture the session id issued by the server (initialize response).
        if let Some(sid) = resp.headers().get("mcp-session-id") {
            if let Ok(s) = sid.to_str() {
                *self.session_id.lock().unwrap() = Some(s.to_string());
            }
        }

        if !expect_response || status == reqwest::StatusCode::ACCEPTED {
            return Ok(Value::Null);
        }
        if !status.is_success() {
            bail!("MCP HTTP error: status {}", status);
        }

        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("application/json")
            .to_string();
        let body = resp.text().await?;
        if content_type.contains("text/event-stream") {
            parse_sse_response(&body)
                .ok_or_else(|| anyhow!("No JSON-RPC message found in SSE response"))
        } else {
            serde_json::from_str(&body)
                .map_err(|e| anyhow!("Invalid JSON from MCP server: {}", e))
        }
    }
}

/// Extract the `result` (or error) of a JSON-RPC response, matching by id.
fn extract_rpc_result(id: u64, msg: &Value) -> Result<Value> {
    if let Some(err) = msg.get("error") {
        let message = err
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("unknown MCP error");
        bail!("MCP error: {}", message);
    }
    if let Some(msg_id) = msg.get("id") {
        if msg_id.as_u64() != Some(id) {
            bail!("MCP response id mismatch: expected {}, got {}", id, msg_id);
        }
    }
    Ok(msg.get("result").cloned().unwrap_or(Value::Null))
}

/// Extract concatenated text content from an MCP `tools/call` result.
fn extract_text(result: &Value) -> String {
    result
        .get("content")
        .and_then(|c| c.as_array())
        .map(|arr| {
            arr.iter()
                .filter(|item| item.get("type").and_then(|t| t.as_str()) == Some("text"))
                .filter_map(|item| item.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

/// Parse a single JSON-RPC message out of an SSE body.
///
/// Handles the common streamable-HTTP shape: `event: message\ndata: {json}`.
pub fn parse_sse_response(body: &str) -> Option<Value> {
    for line in body.lines() {
        let line = line.trim();
        if let Some(data) = line.strip_prefix("data:") {
            let data = data.trim();
            if data.is_empty() {
                continue;
            }
            if let Ok(v) = serde_json::from_str::<Value>(data) {
                if v.get("jsonrpc").is_some() {
                    return Some(v);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_sse_response_single_event() {
        let body = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"tools\":[]}}\n\n";
        let msg = parse_sse_response(body).unwrap();
        assert_eq!(msg["id"], 1);
        assert!(msg["result"]["tools"].is_array());
    }

    #[test]
    fn test_parse_sse_response_ignores_non_jsonrpc_data() {
        let body = "data: {\"hello\":1}\ndata: {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{}}\n";
        let msg = parse_sse_response(body).unwrap();
        assert_eq!(msg["id"], 2);
    }

    #[test]
    fn test_parse_sse_response_no_message() {
        assert!(parse_sse_response("").is_none());
        assert!(parse_sse_response("event: ping\ndata: {}\n").is_none());
    }

    #[test]
    fn test_extract_rpc_result_ok_and_error() {
        let ok = json!({"jsonrpc": "2.0", "id": 5, "result": {"a": 1}});
        assert_eq!(extract_rpc_result(5, &ok).unwrap()["a"], 1);

        let err = json!({"jsonrpc": "2.0", "id": 5, "error": {"code": -1, "message": "boom"}});
        let e = extract_rpc_result(5, &err).unwrap_err();
        assert!(e.to_string().contains("boom"));

        let mismatch = json!({"jsonrpc": "2.0", "id": 6, "result": {}});
        assert!(extract_rpc_result(5, &mismatch).is_err());
    }

    #[test]
    fn test_extract_text() {
        let result = json!({
            "content": [
                {"type": "text", "text": "hello"},
                {"type": "image", "data": "xxx"},
                {"type": "text", "text": "world"}
            ]
        });
        assert_eq!(extract_text(&result), "hello\nworld");
        assert_eq!(extract_text(&json!({})), "");
    }

    #[tokio::test]
    async fn test_http_transport_end_to_end() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        /// Build a raw HTTP response with the given body and content type.
        fn http_response(content_type: &str, extra_headers: &str, body: &str) -> String {
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {}\r\n{}Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                content_type,
                extra_headers,
                body.len(),
                body
            )
        }

        /// Read one full HTTP request (headers + Content-Length body).
        async fn read_request(sock: &mut tokio::net::TcpStream) -> Option<Value> {
            let mut data = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = sock.read(&mut buf).await.ok()?;
                if n == 0 {
                    return None;
                }
                data.extend_from_slice(&buf[..n]);
                let raw = String::from_utf8_lossy(&data);
                if let Some(header_end) = raw.find("\r\n\r\n") {
                    let headers = &raw[..header_end];
                    let content_length: usize = headers
                        .lines()
                        .find_map(|l| {
                            let l = l.to_ascii_lowercase();
                            l.strip_prefix("content-length:").map(|v| v.trim().parse().unwrap_or(0))
                        })
                        .unwrap_or(0);
                    let body_start = header_end + 4;
                    if data.len() >= body_start + content_length {
                        let body = &raw[body_start..body_start + content_length];
                        return serde_json::from_str(body).ok();
                    }
                }
            }
        }

        // Minimal fake MCP server over raw TCP: responds to initialize,
        // notifications/initialized (202), tools/list and tools/call.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            loop {
                let (mut sock, _) = match listener.accept().await {
                    Ok(v) => v,
                    Err(_) => break,
                };
                tokio::spawn(async move {
                    let msg = match read_request(&mut sock).await {
                        Some(m) => m,
                        None => return,
                    };
                    let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
                    let response = match method {
                        "initialize" => {
                            let body = serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": msg["id"],
                                "result": {
                                    "protocolVersion": "2025-03-26",
                                    "capabilities": {},
                                    "serverInfo": {"name": "fake", "version": "1.0"}
                                }
                            })
                            .to_string();
                            http_response(
                                "application/json",
                                "Mcp-Session-Id: test-session\r\n",
                                &body,
                            )
                        }
                        "notifications/initialized" => {
                            "HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                                .to_string()
                        }
                        "tools/list" => {
                            let body = serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": msg["id"],
                                "result": {"tools": [{
                                    "name": "echo",
                                    "description": "Echo back the input",
                                    "inputSchema": {"type": "object"}
                                }]}
                            })
                            .to_string();
                            let sse = format!("event: message\ndata: {}\n\n", body);
                            http_response("text/event-stream", "", &sse)
                        }
                        "tools/call" => {
                            let args = msg["params"]["arguments"].to_string();
                            let body = serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": msg["id"],
                                "result": {"content": [{"type": "text", "text": format!("echo: {}", args)}]}
                            })
                            .to_string();
                            http_response("application/json", "", &body)
                        }
                        _ => return,
                    };
                    let _ = sock.write_all(response.as_bytes()).await;
                    let _ = sock.shutdown().await;
                });
            }
        });

        let url = format!("http://{}/mcp", addr);
        let client = McpClient::connect_http(&url, HashMap::new());
        client.initialize().await.unwrap();

        let tools = client.list_tools().await.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "echo");
        assert_eq!(tools[0].description, "Echo back the input");

        let out = client.call_tool("echo", json!({"msg": "hi"})).await.unwrap();
        assert!(out.contains("echo:"), "unexpected output: {}", out);
        assert!(out.contains("hi"));
    }
}
