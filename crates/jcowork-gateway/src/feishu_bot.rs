//! Persistent Feishu bot connection (常连接).
//!
//! The heavy lifting — the WebSocket to Feishu, reconnects, acks, protobuf
//! framing — is done by the official `lark-cli event consume` command. This
//! module supervises that process per enabled account, parses the NDJSON it
//! prints, runs the agent turn and replies through the IM API.
//!
//! One connection per jcowork user, keyed by user id. It is started when the
//! Feishu skill is enabled (or at boot for accounts that already had it on)
//! and stopped when the skill is turned off.

use anyhow::Result;
use serde::Serialize;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{watch, Mutex};

use jcowork_feishu::event::parse_cli_event;
use jcowork_feishu::lark_cli::{build_cli_env, FEISHU_SKILL_ID};
use jcowork_llm::provider::ChatMessage;

use crate::router::AppState;

/// Initial reconnect delay; doubles up to [`MAX_BACKOFF_SECS`].
const INITIAL_BACKOFF_SECS: u64 = 5;
const MAX_BACKOFF_SECS: u64 = 60;

/// The CLI's env credentials carry a tenant access token, which Feishu expires
/// after ~2 hours. Recycle the stream well before that so a reconnect always
/// uses a freshly minted token.
const RECYCLE_AFTER: Duration = Duration::from_secs(60 * 60);

/// Processed message ids remembered for deduplication (Feishu may redeliver).
const SEEN_CAPACITY: usize = 500;

/// Connection state exposed to the UI.
#[derive(Debug, Clone, Serialize, Default)]
pub struct BotStatus {
    /// "stopped" | "connecting" | "connected" | "error"
    pub state: String,
    /// Short human-readable detail (Chinese, shown as-is in the UI).
    pub detail: Option<String>,
    pub last_error: Option<String>,
    pub app_id: Option<String>,
    /// When the connection was started (RFC 3339).
    pub since: Option<String>,
}

impl BotStatus {
    pub fn stopped() -> Self {
        Self {
            state: "stopped".to_string(),
            detail: Some("未连接".to_string()),
            ..Default::default()
        }
    }
}

struct Connection {
    status: Arc<Mutex<BotStatus>>,
    stop: watch::Sender<bool>,
    abort: tokio::task::AbortHandle,
}

/// Owns one supervised connection per user.
#[derive(Default)]
pub struct FeishuBotManager {
    connections: Mutex<HashMap<String, Connection>>,
}

impl FeishuBotManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn status(&self, user_id: &str) -> BotStatus {
        let entry = {
            let conns = self.connections.lock().await;
            conns.get(user_id).map(|c| c.status.clone())
        };
        match entry {
            Some(status) => status.lock().await.clone(),
            None => BotStatus::stopped(),
        }
    }

    /// Stop and start again, picking up the latest saved credentials.
    pub async fn start(&self, state: &AppState, user_id: &str) -> Result<BotStatus> {
        let config = state
            .feishu_config_store
            .get_by_user(user_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("尚未配置飞书机器人：请先填写 App ID 和 App Secret"))?;
        if config.app_id.trim().is_empty() || config.app_secret.trim().is_empty() {
            anyhow::bail!("飞书 App ID / App Secret 不能为空");
        }

        self.stop(user_id).await;

        let status = Arc::new(Mutex::new(BotStatus {
            state: "connecting".to_string(),
            detail: Some("正在建立飞书长连接…".to_string()),
            last_error: None,
            app_id: Some(config.app_id.clone()),
            since: Some(chrono::Utc::now().to_rfc3339()),
        }));
        let (stop_tx, stop_rx) = watch::channel(false);

        let state_clone = state.clone();
        let owner = user_id.to_string();
        let app_id = config.app_id.clone();
        let app_secret = config.app_secret.clone();
        let status_clone = status.clone();
        let task = tokio::spawn(async move {
            run_bot(state_clone, owner, app_id, app_secret, status_clone, stop_rx).await;
        });

        let initial = status.lock().await.clone();
        self.connections.lock().await.insert(
            user_id.to_string(),
            Connection {
                status,
                stop: stop_tx,
                abort: task.abort_handle(),
            },
        );
        Ok(initial)
    }

    pub async fn stop(&self, user_id: &str) {
        let conn = self.connections.lock().await.remove(user_id);
        if let Some(conn) = conn {
            let _ = conn.stop.send(true);
            // Aborting drops the supervisor future, which drops the child
            // process (`kill_on_drop`), so the CLI never outlives the manager.
            conn.abort.abort();
        }
    }

    /// Restart when the skill is on, stop when it is off.
    pub async fn sync_with_skill(&self, state: &AppState, user_id: &str) {
        if feishu_skill_enabled(state, user_id).await {
            if let Err(e) = self.start(state, user_id).await {
                tracing::warn!(user_id = %user_id, error = %e, "Feishu bot could not start");
            }
        } else {
            self.stop(user_id).await;
        }
    }

    /// Re-open the long connection for every account that had the skill on.
    pub async fn restore(&self, state: &AppState) {
        let entries = match state.feishu_config_store.list_all().await {
            Ok(entries) => entries,
            Err(e) => {
                tracing::warn!(error = %e, "Failed to load Feishu configs for restore");
                return;
            }
        };
        for entry in entries {
            if entry.app_id.trim().is_empty() || entry.app_secret.trim().is_empty() {
                continue;
            }
            if !feishu_skill_enabled(state, &entry.user_id).await {
                continue;
            }
            match self.start(state, &entry.user_id).await {
                Ok(_) => tracing::info!(user_id = %entry.user_id, app_id = %entry.app_id, "Feishu bot restored"),
                Err(e) => tracing::warn!(user_id = %entry.user_id, error = %e, "Feishu bot restore failed"),
            }
        }
    }
}

/// Whether the user has the Feishu skill enabled (memory is the single
/// source of truth shared with the agent's skill gating).
pub async fn feishu_skill_enabled(state: &AppState, user_id: &str) -> bool {
    state
        .memory_manager
        .recall_all(user_id)
        .await
        .map(|entries| {
            entries
                .iter()
                .any(|e| e.category == "skill_enabled" && e.content == FEISHU_SKILL_ID)
        })
        .unwrap_or(false)
}

/// Supervise one `lark-cli event consume` process for the lifetime of the skill.
async fn run_bot(
    state: AppState,
    user_id: String,
    app_id: String,
    app_secret: String,
    status: Arc<Mutex<BotStatus>>,
    mut stop_rx: watch::Receiver<bool>,
) {
    let client = crate::feishu::client_for(&state, &app_id, &app_secret);
    let mut histories: HashMap<String, Vec<ChatMessage>> = HashMap::new();
    let mut seen_order: VecDeque<String> = VecDeque::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut backoff = INITIAL_BACKOFF_SECS;

    loop {
        if *stop_rx.borrow() {
            return;
        }

        // The CLI must be installed; the install itself is triggered from the
        // skill card, so here we just wait for it.
        let cli = state.lark_cli.refresh().await;
        if cli.state != "installed" {
            set_status(
                &status,
                "error",
                Some("lark-cli 尚未安装".to_string()),
                Some(
                    cli.error
                        .clone()
                        .unwrap_or_else(|| "请在「技能」页面的飞书卡片里安装 lark-cli".to_string()),
                ),
            )
            .await;
            if sleep_or_stopped(&mut stop_rx, 30).await {
                return;
            }
            continue;
        }
        let Some(bin) = state.lark_cli.resolve_binary() else {
            if sleep_or_stopped(&mut stop_rx, 30).await {
                return;
            }
            continue;
        };
        if let Err(e) = state.lark_cli.prepare_dirs().await {
            tracing::warn!(error = %e, "Failed to prepare lark-cli dirs");
        }

        let token = match client.tenant_token().await {
            Ok(token) => token,
            Err(e) => {
                set_status(
                    &status,
                    "error",
                    Some("飞书凭证校验失败".to_string()),
                    Some(format!("{}", e)),
                )
                .await;
                if sleep_or_stopped(&mut stop_rx, backoff).await {
                    return;
                }
                backoff = (backoff * 2).min(MAX_BACKOFF_SECS);
                continue;
            }
        };
        let env = build_cli_env(&app_id, &app_secret, &token, &state.lark_cli.config_dir());

        let mut cmd = tokio::process::Command::new(&bin);
        cmd.arg("event")
            .arg("consume")
            .arg("im.message.receive_v1")
            .arg("--as")
            .arg("bot")
            .current_dir(&state.data_dir)
            .envs(env.iter().map(|(k, v)| (k.clone(), v.clone())))
            .kill_on_drop(true)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(e) => {
                set_status(
                    &status,
                    "error",
                    Some("无法启动 lark-cli".to_string()),
                    Some(e.to_string()),
                )
                .await;
                if sleep_or_stopped(&mut stop_rx, backoff).await {
                    return;
                }
                backoff = (backoff * 2).min(MAX_BACKOFF_SECS);
                continue;
            }
        };

        // Drain stderr into a small ring so the pipe never fills up and the
        // last lines are available when the process fails.
        let stderr_tail = Arc::new(Mutex::new(VecDeque::<String>::new()));
        if let Some(stderr) = child.stderr.take() {
            let tail = stderr_tail.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let mut tail = tail.lock().await;
                    if tail.len() >= 20 {
                        tail.pop_front();
                    }
                    tail.push_back(line);
                }
            });
        }

        let stdout = match child.stdout.take() {
            Some(stdout) => stdout,
            None => {
                let _ = child.kill().await;
                continue;
            }
        };
        let mut lines = BufReader::new(stdout).lines();
        let mut connected = false;
        let recycle = tokio::time::sleep(RECYCLE_AFTER);
        tokio::pin!(recycle);

        loop {
            tokio::select! {
                _ = stop_rx.changed() => {
                    if *stop_rx.borrow() {
                        let _ = child.kill().await;
                        set_status(&status, "stopped", Some("已断开".to_string()), None).await;
                        return;
                    }
                }
                _ = &mut recycle, if connected => {
                    tracing::info!(app_id = %app_id, "Recycling Feishu event stream to refresh the tenant token");
                    let _ = child.kill().await;
                    break;
                }
                line = lines.next_line() => {
                    match line {
                        Ok(Some(line)) => {
                            if !connected {
                                connected = true;
                                backoff = INITIAL_BACKOFF_SECS;
                                set_status(
                                    &status,
                                    "connected",
                                    Some("已连接，等待飞书消息".to_string()),
                                    None,
                                )
                                .await;
                            }
                            if let Some(ev) = parse_cli_event(&line) {
                                if seen.insert(ev.message_id.clone()) {
                                    seen_order.push_back(ev.message_id.clone());
                                    if seen_order.len() > SEEN_CAPACITY {
                                        if let Some(old) = seen_order.pop_front() {
                                            seen.remove(&old);
                                        }
                                    }
                                    if ev.should_answer() {
                                        let history = histories.entry(ev.chat_id.clone()).or_default();
                                        let outcome = crate::feishu::feishu_turn(
                                            &state, &user_id, &ev.content, history,
                                        ).await;
                                        let reply = match outcome {
                                            Ok(reply) => reply,
                                            Err(e) => {
                                                tracing::warn!(
                                                    app_id = %app_id,
                                                    message_id = %ev.message_id,
                                                    error = %e,
                                                    "Feishu bot turn failed"
                                                );
                                                format!("抱歉，处理这条消息时出错：{}", e)
                                            }
                                        };
                                        if let Err(e) = client.reply_message(&ev.message_id, &reply).await {
                                            tracing::warn!(
                                                message_id = %ev.message_id,
                                                error = %e,
                                                "Feishu bot reply failed"
                                            );
                                        }
                                    }
                                }
                            }
                        }
                        Ok(None) => break,
                        Err(e) => {
                            tracing::warn!(error = %e, "Feishu event stream read error");
                            break;
                        }
                    }
                }
            }
        }

        let exit_code = child.wait().await.ok().and_then(|s| s.code());

        // A recycled stream is a normal restart: no error banner.
        if exit_code.is_none() && !*stop_rx.borrow() {
            set_status(
                &status,
                "connecting",
                Some("正在重新连接…".to_string()),
                None,
            )
            .await;
            if sleep_or_stopped(&mut stop_rx, 1).await {
                return;
            }
            continue;
        }
        if *stop_rx.borrow() {
            return;
        }

        let detail = {
            let tail = stderr_tail.lock().await;
            tail.iter()
                .rev()
                .find(|l| !l.trim().is_empty())
                .cloned()
                .unwrap_or_else(|| "飞书长连接已断开".to_string())
        };
        set_status(
            &status,
            "error",
            Some(format!("连接断开（退出码 {}），{}s 后重试", exit_code.unwrap_or(-1), backoff)),
            Some(detail),
        )
        .await;
        if sleep_or_stopped(&mut stop_rx, backoff).await {
            return;
        }
        backoff = (backoff * 2).min(MAX_BACKOFF_SECS);
    }
}

async fn set_status(
    status: &Arc<Mutex<BotStatus>>,
    state: &str,
    detail: Option<String>,
    last_error: Option<String>,
) {
    let mut status = status.lock().await;
    status.state = state.to_string();
    status.detail = detail;
    if last_error.is_some() {
        status.last_error = last_error;
    }
}

/// Sleep for `secs`, returning `true` when the connection was asked to stop.
async fn sleep_or_stopped(stop_rx: &mut watch::Receiver<bool>, secs: u64) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(Duration::from_secs(secs)) => *stop_rx.borrow(),
        changed = stop_rx.changed() => changed.is_err() || *stop_rx.borrow(),
    }
}
