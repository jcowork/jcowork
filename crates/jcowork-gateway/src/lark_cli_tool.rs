//! `lark_cli` tool — lets the agent drive the official Lark/Feishu CLI.
//!
//! The CLI is the same binary the Feishu skill installs for its long
//! connection. Credentials come from the user's saved Feishu app config and
//! are injected through `LARKSUITE_CLI_*` environment variables, so the tool
//! never has to pass secrets on the command line (and the model cannot see
//! them by echoing `args`).

use anyhow::Result;
use async_trait::async_trait;
use dashmap::DashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use jcowork_feishu::client::FeishuClient;
use jcowork_feishu::lark_cli::{build_cli_env, LarkCliRunner};
use jcowork_storage::FeishuConfigStore;
use jcowork_tools::base::{Tool, ToolContext};

const DEFAULT_TIMEOUT_SECS: u64 = 120;
const MAX_TIMEOUT_SECS: u64 = 600;

/// Subcommands the host owns: they would replace credentials, start a
/// competing event connection, or rewrite the managed install.
const BLOCKED_SUBCOMMANDS: &[&str] = &["config", "auth", "event", "update", "profile", "doctor"];

/// Run the official lark-cli as a tool.
pub struct LarkCliTool {
    runner: LarkCliRunner,
    configs: Arc<FeishuConfigStore>,
    clients: Arc<DashMap<String, Arc<FeishuClient>>>,
}

impl LarkCliTool {
    pub fn new(
        runner: LarkCliRunner,
        configs: Arc<FeishuConfigStore>,
        clients: Arc<DashMap<String, Arc<FeishuClient>>>,
    ) -> Self {
        Self {
            runner,
            configs,
            clients,
        }
    }
}

#[async_trait]
impl Tool for LarkCliTool {
    fn name(&self) -> &str {
        "lark_cli"
    }

    fn description(&self) -> &str {
        "Run the official Lark/Feishu CLI (`lark-cli`) as the user's bot. Use it to create, \
         fetch, search and update Feishu docs, sheets, wiki pages, drive files, calendar events, \
         tasks, or to send IM messages. The CLI is run inside the user's workspace, so `@file` \
         arguments (e.g. `--content @notes.md`) read workspace files. Credentials are injected by \
         the host — never pass `--as`, tokens, or run `config`/`auth`/`event` subcommands. \
         When unsure about flags, call with `[\"<domain>\", \"--help\"]` first."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "args": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Arguments passed to lark-cli in order, e.g. [\"docs\", \"+create\", \"--title\", \"Title\", \"--doc-format\", \"markdown\", \"--content\", \"@doc.md\"]"
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": "Optional timeout in seconds (default 120, max 600)"
                }
            },
            "required": ["args"]
        })
    }

    async fn execute(&self, args: &str, ctx: &ToolContext) -> Result<String> {
        let parsed: serde_json::Value = serde_json::from_str(args)
            .map_err(|e| anyhow::anyhow!("参数必须是 JSON 对象：{}", e))?;

        let raw_args = parsed
            .get("args")
            .and_then(|v| v.as_array())
            .ok_or_else(|| anyhow::anyhow!("缺少 'args' 数组参数"))?;
        let mut cli_args: Vec<String> = Vec::with_capacity(raw_args.len());
        for value in raw_args {
            let arg = value
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("'args' 中的每一项都必须是字符串"))?;
            cli_args.push(arg.to_string());
        }
        if cli_args.is_empty() {
            anyhow::bail!("'args' 不能为空");
        }

        // The model must not be able to change identity or the install itself.
        let first = cli_args[0].as_str();
        if BLOCKED_SUBCOMMANDS.contains(&first) {
            anyhow::bail!(
                "不支持 `lark-cli {}` 子命令：凭证和长连接由宿主管理",
                first
            );
        }
        if cli_args.iter().any(|a| a == "--as") {
            anyhow::bail!("不要传 --as：身份固定为机器人（bot）");
        }

        let timeout_secs = parsed
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_TIMEOUT_SECS)
            .clamp(5, MAX_TIMEOUT_SECS);

        // Resolve the user's bot credentials.
        let config = self
            .configs
            .get_by_user(&ctx.user_id)
            .await?
            .ok_or_else(|| {
                anyhow::anyhow!("尚未配置飞书机器人：请在「技能」页面的飞书技能里填写 App ID 和 App Secret")
            })?;
        if config.app_id.trim().is_empty() || config.app_secret.trim().is_empty() {
            anyhow::bail!("飞书 App ID / App Secret 不完整，请先在技能页面补全");
        }

        let client = self
            .clients
            .entry(config.app_id.clone())
            .or_insert_with(|| {
                Arc::new(FeishuClient::new(
                    config.app_id.clone(),
                    config.app_secret.clone(),
                ))
            })
            .clone();
        let token = client
            .tenant_token()
            .await
            .map_err(|e| anyhow::anyhow!("获取 tenant access token 失败：{}", e))?;

        let env = build_cli_env(
            &config.app_id,
            &config.app_secret,
            &token,
            &self.runner.config_dir(),
        );

        // The agent usually works inside its workspace; fall back to the
        // managed install dir on the off chance the workspace has not been
        // created yet.
        let workspace = PathBuf::from(&ctx.workspace_root);
        let cwd = if workspace.is_dir() {
            workspace
        } else {
            self.runner.managed_prefix()
        };

        let output = self
            .runner
            .run(&cli_args, &env, &cwd, Duration::from_secs(timeout_secs))
            .await?;

        let mut text = String::new();
        if !output.stdout.trim().is_empty() {
            text.push_str(output.stdout.trim_end());
        }
        if !output.stderr.trim().is_empty() {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str("[stderr] ");
            text.push_str(output.stderr.trim_end());
        }
        match output.code {
            Some(0) => Ok(if text.trim().is_empty() {
                "（命令执行成功，无输出）".to_string()
            } else {
                text
            }),
            Some(code) => Ok(format!("[exit code {}] {}", code, text)),
            None => Ok(format!("[进程被信号终止] {}", text)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn ctx(dir: &Path) -> ToolContext {
        ToolContext {
            user_id: "u1".to_string(),
            workspace_root: dir.to_string_lossy().to_string(),
            mentioned_public_users: Vec::new(),
            model: None,
        }
    }

    #[tokio::test]
    async fn test_rejects_blocked_subcommand_without_cli() {
        // No CLI installed and no config: the argument validation must fire
        // before either becomes relevant.
        let dir = tempfile::tempdir().unwrap();
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        let store = Arc::new(FeishuConfigStore::new(pool));
        let tool = LarkCliTool::new(
            LarkCliRunner::new(dir.path().to_string_lossy().to_string()),
            store,
            Arc::new(DashMap::new()),
        );

        let err = tool
            .execute(r#"{"args":["config","init"]}"#, &ctx(dir.path()))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("不支持"));

        let err = tool
            .execute(r#"{"args":["docs","+fetch","--as","user"]}"#, &ctx(dir.path()))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("--as"));

        let err = tool
            .execute(r#"{"args":[]}"#, &ctx(dir.path()))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("不能为空"));
    }
}
