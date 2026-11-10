//! Official Lark/Feishu CLI (`@larksuite/cli`) integration.
//!
//! The Feishu skill is bound to the official CLI: the CLI keeps the bot's
//! real-time event stream open (`lark-cli event consume im.message.receive_v1`)
//! and lets the agent create/read docs, sheets, tasks and so on.
//!
//! The CLI is installed into the app data directory (not globally) so a
//! desktop install stays self-contained and does not depend on a writable
//! `/usr/local`. Credentials are injected per invocation through the
//! documented `LARKSUITE_CLI_*` environment variables — the CLI's own config
//! is a single OS-user-wide store, which would leak between jcowork accounts.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

/// Built-in skill that owns the bot + CLI.
pub const FEISHU_SKILL_ID: &str = "builtin:feishu";

/// npm package of the official CLI.
const LARK_CLI_PACKAGE: &str = "@larksuite/cli";

/// Sub-directory of the app data dir that holds the managed install.
const MANAGED_DIR: &str = "lark-cli";

/// Max bytes of CLI output handed back to the model.
const MAX_OUTPUT_BYTES: usize = 64 * 1024;

/// Install / probe state of the CLI.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LarkCliStatus {
    /// `installed` | `installing` | `missing` | `failed`
    pub state: String,
    pub path: Option<String>,
    pub version: Option<String>,
    pub error: Option<String>,
}

impl Default for LarkCliStatus {
    fn default() -> Self {
        Self {
            state: "missing".to_string(),
            path: None,
            version: None,
            error: None,
        }
    }
}

/// Result of one CLI invocation.
#[derive(Debug, Clone)]
pub struct LarkCliOutput {
    pub stdout: String,
    pub stderr: String,
    pub code: Option<i32>,
}

/// Installs, resolves and runs the official CLI.
///
/// Cheap to clone (all state is shared), so one runner can be held by the
/// tool registry, the bot manager and the HTTP layer at the same time.
#[derive(Clone)]
pub struct LarkCliRunner {
    data_dir: String,
    status: Arc<Mutex<LarkCliStatus>>,
    install_lock: Arc<Mutex<()>>,
}

impl LarkCliRunner {
    pub fn new(data_dir: impl Into<String>) -> Self {
        Self {
            data_dir: data_dir.into(),
            status: Arc::new(Mutex::new(LarkCliStatus::default())),
            install_lock: Arc::new(Mutex::new(())),
        }
    }

    /// Directory of the managed npm install.
    pub fn managed_prefix(&self) -> PathBuf {
        Path::new(&self.data_dir).join(MANAGED_DIR)
    }

    /// Path of the downloaded native binary inside the managed install.
    pub fn managed_binary(&self) -> PathBuf {
        let name = if cfg!(windows) {
            "lark-cli.exe"
        } else {
            "lark-cli"
        };
        self.managed_prefix()
            .join("node_modules")
            .join("@larksuite")
            .join("cli")
            .join("bin")
            .join(name)
    }

    /// Private config dir. The CLI otherwise stores its config under the OS
    /// user's home and keeps one event-bus daemon per app there; pinning the
    /// dir keeps jcowork's bot isolated from a user's own lark-cli install.
    pub fn config_dir(&self) -> PathBuf {
        self.managed_prefix().join("config")
    }

    /// Private log dir for the CLI and its event-bus daemon.
    pub fn log_dir(&self) -> PathBuf {
        self.managed_prefix().join("logs")
    }

    /// Create the directories the CLI expects to write into.
    pub async fn prepare_dirs(&self) -> Result<()> {
        tokio::fs::create_dir_all(self.config_dir()).await?;
        tokio::fs::create_dir_all(self.log_dir()).await?;
        Ok(())
    }

    pub async fn status(&self) -> LarkCliStatus {
        self.status.lock().await.clone()
    }

    /// Locate the CLI: explicit override, managed install, then `PATH`.
    pub fn resolve_binary(&self) -> Option<PathBuf> {
        if let Ok(explicit) = std::env::var("JCWORK_LARK_CLI_BIN") {
            let p = PathBuf::from(explicit);
            if p.is_file() {
                return Some(p);
            }
        }
        let managed = self.managed_binary();
        if managed.is_file() {
            return Some(managed);
        }
        find_in_dirs(&search_dirs(), cli_binary_name())
    }

    /// Re-probe the CLI and cache the outcome.
    pub async fn refresh(&self) -> LarkCliStatus {
        let previous = self.status.lock().await.clone();
        let Some(path) = self.resolve_binary() else {
            let mut st = self.status.lock().await;
            // A running install has no binary yet; keep reporting progress so
            // UI polls do not flip the state back to "missing". A recorded
            // failure keeps its error for the skill card instead of silently
            // looking uninstalled again; retrying goes through `install()`.
            if st.state == "installing" || st.state == "failed" {
                return st.clone();
            }
            let next = LarkCliStatus {
                state: "missing".to_string(),
                ..Default::default()
            };
            *st = next.clone();
            return next;
        };

        // Installed binaries rarely change; keep the probed version.
        if previous.state == "installed"
            && previous.version.is_some()
            && previous.path.as_deref() == Some(path.to_string_lossy().as_ref())
            && path.is_file()
        {
            return previous;
        }

        let next = match probe_version(&path).await {
            Ok(version) => LarkCliStatus {
                state: "installed".to_string(),
                path: Some(path.to_string_lossy().to_string()),
                version: Some(version),
                error: None,
            },
            Err(e) => LarkCliStatus {
                state: "failed".to_string(),
                path: Some(path.to_string_lossy().to_string()),
                version: None,
                error: Some(format!("{e:#}")),
            },
        };
        *self.status.lock().await = next.clone();
        next
    }

    /// Ensure the CLI exists, installing it into the app data dir when needed.
    ///
    /// Safe to call concurrently: the second caller waits for the running
    /// install instead of starting a competing `npm install`.
    pub async fn ensure_installed(&self) -> LarkCliStatus {
        if self.resolve_binary().is_some() {
            return self.refresh().await;
        }
        self.install().await
    }

    /// Download and install the CLI. Returns the post-install status.
    pub async fn install(&self) -> LarkCliStatus {
        let _guard = self.install_lock.lock().await;

        // A concurrent caller may have completed the install while we waited.
        if self.resolve_binary().is_some() {
            return self.refresh().await;
        }

        {
            let mut st = self.status.lock().await;
            *st = LarkCliStatus {
                state: "installing".to_string(),
                error: None,
                ..Default::default()
            };
        }

        let result = self.run_install().await;
        let next = match result {
            Ok(()) => self.refresh().await,
            Err(e) => LarkCliStatus {
                state: "failed".to_string(),
                path: None,
                version: None,
                error: Some(format!("{e:#}")),
            },
        };
        *self.status.lock().await = next.clone();
        next
    }

    async fn run_install(&self) -> Result<()> {
        let npm = find_npm().context(
            "未找到 npm：请先安装 Node.js（https://nodejs.org），再重新安装 lark-cli",
        )?;
        // A desktop app launched from Finder gets a minimal PATH
        // (`/usr/bin:/bin:...`). npm is a `#!/usr/bin/env node` script, so
        // spawning it otherwise fails with "env: node: No such file or
        // directory" even though npm itself was found via search_dirs().
        let node = find_node().context(
            "未找到 node：请先安装 Node.js（https://nodejs.org），再重新安装 lark-cli",
        )?;
        let path_env = install_path(&npm, &node);
        let prefix = self.managed_prefix();
        tokio::fs::create_dir_all(&prefix).await?;
        let cache = prefix.join(".npm-cache");
        tokio::fs::create_dir_all(&cache).await?;

        // `--prefix` keeps the install inside the app data dir; the private
        // cache sidesteps a broken/shared ~/.npm on the user's machine.
        let mut cmd = tokio::process::Command::new(&npm);
        cmd.arg("install")
            .arg("--prefix")
            .arg(&prefix)
            .arg("--no-save")
            .arg("--no-audit")
            .arg("--no-fund")
            .arg("--loglevel=error")
            .arg("--cache")
            .arg(&cache)
            .arg(format!("{}@latest", LARK_CLI_PACKAGE))
            .env("PATH", &path_env)
            .env("npm_config_cache", &cache)
            .kill_on_drop(true)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        tracing::info!(npm = %npm.display(), prefix = %prefix.display(), "Installing lark-cli");
        let output = tokio::time::timeout(Duration::from_secs(600), cmd.output())
            .await
            .context("安装 lark-cli 超时（10 分钟）")?
            .context("无法启动 npm")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!(
                "npm install {} 失败：{}",
                LARK_CLI_PACKAGE,
                tail(&stderr, 600)
            );
        }
        if !self.managed_binary().is_file() {
            anyhow::bail!(
                "安装完成但未找到 lark-cli 二进制：{}",
                self.managed_binary().display()
            );
        }
        Ok(())
    }

    /// Run the CLI with credentials injected through the environment.
    pub async fn run(
        &self,
        args: &[String],
        extra_env: &[(String, String)],
        cwd: &Path,
        timeout: Duration,
    ) -> Result<LarkCliOutput> {
        let bin = self
            .resolve_binary()
            .context("lark-cli 尚未安装，请在「技能」页面点击安装")?;
        self.prepare_dirs().await?;

        let mut cmd = tokio::process::Command::new(&bin);
        cmd.args(args)
            .current_dir(cwd)
            .envs(extra_env.iter().map(|(k, v)| (k.clone(), v.clone())))
            .kill_on_drop(true)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        let output = tokio::time::timeout(timeout, cmd.output())
            .await
            .with_context(|| format!("lark-cli 执行超时（{}s）", timeout.as_secs()))?
            .with_context(|| format!("无法启动 lark-cli：{}", bin.display()))?;

        Ok(LarkCliOutput {
            stdout: truncate(&String::from_utf8_lossy(&output.stdout)),
            stderr: truncate(&String::from_utf8_lossy(&output.stderr)),
            code: output.status.code(),
        })
    }
}

/// Environment handed to every CLI invocation.
///
/// The CLI's env credential provider only accepts a bot identity when a
/// tenant access token is supplied; app id + secret alone are not enough.
pub fn build_cli_env(
    app_id: &str,
    app_secret: &str,
    tenant_token: &str,
    config_dir: &Path,
) -> Vec<(String, String)> {
    vec![
        ("LARKSUITE_CLI_APP_ID".to_string(), app_id.to_string()),
        ("LARKSUITE_CLI_APP_SECRET".to_string(), app_secret.to_string()),
        (
            "LARKSUITE_CLI_TENANT_ACCESS_TOKEN".to_string(),
            tenant_token.to_string(),
        ),
        ("LARKSUITE_CLI_STRICT_MODE".to_string(), "bot".to_string()),
        ("LARKSUITE_CLI_DEFAULT_AS".to_string(), "bot".to_string()),
        (
            "LARKSUITE_CLI_CONFIG_DIR".to_string(),
            config_dir.to_string_lossy().to_string(),
        ),
        (
            "LARKSUITE_CLI_LOG_DIR".to_string(),
            config_dir.join("logs").to_string_lossy().to_string(),
        ),
        (
            "LARKSUITE_CLI_NO_UPDATE_NOTIFIER".to_string(),
            "1".to_string(),
        ),
        (
            "LARKSUITE_CLI_NO_SKILLS_NOTIFIER".to_string(),
            "1".to_string(),
        ),
    ]
}

fn cli_binary_name() -> &'static str {
    if cfg!(windows) {
        "lark-cli.exe"
    } else {
        "lark-cli"
    }
}

/// Directories searched for the CLI / npm when they are not in `PATH`.
fn search_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    if let Some(home) = home_dir() {
        dirs.push(home.join(".local/bin"));
        dirs.push(home.join(".npm-global/bin"));
    }
    dirs.push(PathBuf::from("/opt/homebrew/bin"));
    dirs.push(PathBuf::from("/usr/local/bin"));
    dirs
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn find_in_dirs(dirs: &[PathBuf], name: &str) -> Option<PathBuf> {
    dirs.iter()
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

fn find_npm() -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var("JCWORK_NPM_BIN") {
        let p = PathBuf::from(explicit);
        if p.is_file() {
            return Some(p);
        }
    }
    find_in_dirs(&search_dirs(), if cfg!(windows) { "npm.cmd" } else { "npm" })
}

fn find_node() -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var("JCWORK_NODE_BIN") {
        let p = PathBuf::from(explicit);
        if p.is_file() {
            return Some(p);
        }
    }
    find_in_dirs(&search_dirs(), if cfg!(windows) { "node.exe" } else { "node" })
}

/// PATH for the npm child process: the npm and node directories first, then
/// the inherited PATH so npm lifecycle scripts keep working.
fn install_path(npm: &Path, node: &Path) -> std::ffi::OsString {
    let mut dirs: Vec<PathBuf> = Vec::new();
    for dir in [npm.parent(), node.parent()].into_iter().flatten() {
        if !dirs.iter().any(|d| d == dir) {
            dirs.push(dir.to_path_buf());
        }
    }
    if let Some(existing) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&existing));
    }
    std::env::join_paths(dirs).unwrap_or_default()
}

async fn probe_version(bin: &Path) -> Result<String> {
    let mut cmd = tokio::process::Command::new(bin);
    cmd.arg("--version")
        .kill_on_drop(true)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let output = tokio::time::timeout(Duration::from_secs(20), cmd.output())
        .await
        .context("lark-cli --version 超时")?
        .context("无法执行 lark-cli --version")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        anyhow::bail!("lark-cli --version 失败：{}", tail(&stderr, 300));
    }
    parse_version(&stdout)
        .or_else(|| parse_version(&stderr))
        .context("无法解析 lark-cli 版本")
}

/// Extract `x.y.z` from `lark-cli version 1.0.99`.
pub fn parse_version(output: &str) -> Option<String> {
    output
        .split(|c: char| c.is_whitespace() || c == 'v')
        .find(|part| {
            let parts: Vec<&str> = part.split('.').collect();
            parts.len() == 3
                && parts
                    .iter()
                    .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
        })
        .map(|s| s.to_string())
}

fn truncate(s: &str) -> String {
    if s.len() <= MAX_OUTPUT_BYTES {
        return s.to_string();
    }
    let head = jcowork_tools::base::truncate_str(s, MAX_OUTPUT_BYTES);
    format!("{}\n...（输出过长已截断）", head)
}

fn tail(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.trim().to_string();
    }
    let start = s.len() - max;
    let start = (start..s.len()).find(|i| s.is_char_boundary(*i)).unwrap_or(0);
    format!("…{}", s[start..].trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_version() {
        assert_eq!(
            parse_version("lark-cli version 1.0.99"),
            Some("1.0.99".to_string())
        );
        assert_eq!(parse_version("v2.3.4\n"), Some("2.3.4".to_string()));
        assert_eq!(parse_version("no version here"), None);
    }

    #[test]
    fn test_build_cli_env_marks_bot_identity() {
        let env = build_cli_env("cli_a", "secret", "t-token", Path::new("/tmp/larkcfg"));
        let get = |k: &str| {
            env.iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.as_str())
                .unwrap_or("")
        };
        assert_eq!(get("LARKSUITE_CLI_APP_ID"), "cli_a");
        assert_eq!(get("LARKSUITE_CLI_APP_SECRET"), "secret");
        assert_eq!(get("LARKSUITE_CLI_TENANT_ACCESS_TOKEN"), "t-token");
        assert_eq!(get("LARKSUITE_CLI_STRICT_MODE"), "bot");
        assert_eq!(get("LARKSUITE_CLI_DEFAULT_AS"), "bot");
        assert_eq!(get("LARKSUITE_CLI_CONFIG_DIR"), "/tmp/larkcfg");
    }

    #[test]
    fn test_install_path_puts_tool_dirs_first() {
        let path = install_path(
            Path::new("/tmp/jcowork-npm/bin/npm"),
            Path::new("/tmp/jcowork-node/bin/node"),
        );
        let dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
        assert_eq!(dirs.first(), Some(&PathBuf::from("/tmp/jcowork-npm/bin")));
        assert_eq!(dirs.get(1), Some(&PathBuf::from("/tmp/jcowork-node/bin")));
    }

    #[test]
    fn test_install_path_dedupes_shared_dir() {
        let path = install_path(
            Path::new("/tmp/jcowork-shared/npm"),
            Path::new("/tmp/jcowork-shared/node"),
        );
        let dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
        assert_eq!(dirs.first(), Some(&PathBuf::from("/tmp/jcowork-shared")));
        // node lives in the same dir as npm: no duplicate entry is added.
        assert_ne!(dirs.get(1), Some(&PathBuf::from("/tmp/jcowork-shared")));
    }

    #[test]
    fn test_find_in_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join(cli_binary_name());
        std::fs::write(&bin, b"#!/bin/sh\necho ok\n").unwrap();
        let dirs = vec![dir.path().to_path_buf()];
        assert_eq!(find_in_dirs(&dirs, cli_binary_name()), Some(bin));
        assert_eq!(find_in_dirs(&dirs, "does-not-exist"), None);
    }

    #[test]
    fn test_resolve_prefers_managed_install() {
        let dir = tempfile::tempdir().unwrap();
        let runner = LarkCliRunner::new(dir.path().to_string_lossy().to_string());
        assert_eq!(runner.resolve_binary(), None);
        let managed = runner.managed_binary();
        std::fs::create_dir_all(managed.parent().unwrap()).unwrap();
        std::fs::write(&managed, b"binary").unwrap();
        assert_eq!(runner.resolve_binary(), Some(managed));
    }
}
