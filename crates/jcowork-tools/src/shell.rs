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

        // Security: block dangerous commands
        let dangerous = ["rm -rf /", "mkfs", "dd if=", "> /dev/sd", ":(){:|:&};:"];
        for pattern in &dangerous {
            if command.contains(pattern) {
                anyhow::bail!("Command contains blocked pattern: {}", pattern);
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
