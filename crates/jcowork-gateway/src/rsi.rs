//! RSI (recursive self-improvement) — daily, opt-in self-analysis.
//!
//! Unlike periodic tasks, an RSI run does not wait for the user to write a
//! prompt: it derives the prompt from what actually happened in the last 24
//! hours and writes the result back as improvements.
//!
//! Two scopes, decided by the account flag:
//! - **admin**: reads every account's daily usage (LLM requests, tokens,
//!   tool calls/failures, periodic-task outcomes) and proposes improvements
//!   to the project code. Proposals only — the run cannot write outside its
//!   own workspace.
//! - **user**: reads the account's own usage, workspace tree and periodic
//!   plans, then updates the workflows/code in `rsi/` inside the workspace
//!   and may adjust one periodic plan.
//!
//! Runs are recorded in `rsi_reports` and mirrored into the workspace as a
//! markdown file so they show up in Documents.
//!
//! The admin run's source checkout is configurable at runtime
//! (`app_settings.rsi_source_root`) and falls back to auto-detection;
//! see [`RsiService::source_root`].

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::RwLock;
use std::time::Duration;

use anyhow::Result;
use chrono::{DateTime, Local, TimeZone, Timelike, Utc};
use dashmap::DashSet;
use futures::FutureExt;
use jcowork_agent::r#loop as agent_loop;
use jcowork_agent::r#loop::{AgentOutputSink, AgentTurnOptions};
use jcowork_cron::CronScheduler;
use jcowork_llm::LlmRouter;
use jcowork_logs::LogWriter;
use jcowork_storage::rsi_store::{RsiReport, RsiStore};
use jcowork_storage::user_store::User;
use jcowork_storage::{AppSettingsStore, UserSettingsStore, UserStore};
use jcowork_tools::base::ToolContext;
use jcowork_tools::registry::ToolRegistry;

/// Local hour after which the daily run may fire (0-23, default 03:00).
const DEFAULT_RSI_HOUR: u32 = 3;
/// How often the scheduler wakes up to check whether a run is due.
const SCHEDULER_TICK: Duration = Duration::from_secs(30 * 60);
/// Analysed window length.
const WINDOW_HOURS: i64 = 24;
/// Bytes read from the tail of one log file. A day of logs can reach the
/// 1 GB rotation limit; only the newest entries matter for a 24h window.
const MAX_LOG_BYTES: u64 = 32 * 1024 * 1024;
/// Agent turn budget. A daily self-review should not be an endless session.
const MAX_TURNS: usize = 24;
/// Character cap for a workspace tree injected into the prompt.
const WORKSPACE_TREE_MAX: usize = 4000;
/// Character cap for the project tree snapshot.
const PROJECT_TREE_MAX: usize = 6000;
/// `app_settings` key holding the admin-configured source checkout path.
/// Absent/empty means "auto-detect".
pub(crate) const SOURCE_SETTINGS_KEY: &str = "rsi_source_root";

/// Tools a normal-user RSI run may use. Deliberately excludes delete/move/
/// shell: an autonomous run improves files, it does not get to run commands.
const USER_TOOLS: &[&str] = &[
    "file_read",
    "file_write",
    "file_list",
    "file_search",
    "dir_create",
    "dir_list",
    "file_info",
    "cron_add",
    "cron_list",
    "cron_remove",
    "skill_view",
    "skill_search",
    "skill_manage",
    "memory_save",
];

/// Tools an admin RSI run may use. The admin account inspects the checkout
/// read-only via the `source/` prefix and writes its improvement plan into
/// its own workspace; it cannot modify the source tree.
const ADMIN_TOOLS: &[&str] = &[
    "file_read",
    "file_write",
    "file_list",
    "file_search",
    "file_info",
    "dir_list",
];

/// Aggregated usage of one account over the analysed window.
#[derive(Debug, Default, Clone)]
pub struct UsageAgg {
    pub llm_requests: u64,
    pub total_tokens: u64,
    pub tool_calls: u64,
    pub tool_errors: u64,
    pub tools: BTreeMap<String, u64>,
}

impl UsageAgg {
    /// Fold one JSONL log line into the aggregate. Returns false when the
    /// line is not a usage event or falls outside the window, which makes
    /// the parser easy to test line by line.
    pub(crate) fn absorb(&mut self, line: &str, since: DateTime<Utc>) -> bool {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            return false;
        };
        let Some(timestamp) = value.get("timestamp").and_then(|t| t.as_str()) else {
            return false;
        };
        let Ok(at) = DateTime::parse_from_rfc3339(timestamp) else {
            return false;
        };
        if at.with_timezone(&Utc) < since {
            return false;
        }

        match value.get("event").and_then(|e| e.as_str()) {
            Some("llm_request") => {
                self.llm_requests += 1;
                self.total_tokens += value
                    .pointer("/output/usage/total_tokens")
                    .and_then(|t| t.as_u64())
                    .unwrap_or(0);
            }
            Some("tool_call") => {
                self.tool_calls += 1;
                let tool = value
                    .get("tool")
                    .and_then(|t| t.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                *self.tools.entry(tool).or_insert(0) += 1;
                let failed = value
                    .get("output")
                    .and_then(|o| o.as_str())
                    .map(|o| o.starts_with("Error:"))
                    .unwrap_or(false);
                if failed {
                    self.tool_errors += 1;
                }
            }
            _ => return false,
        }
        true
    }

    /// `file_read(6), web_search(3)` — top tools, most used first.
    fn top_tools(&self, limit: usize) -> String {
        let mut tools: Vec<(&String, &u64)> = self.tools.iter().collect();
        tools.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        if tools.is_empty() {
            return "—".to_string();
        }
        tools
            .iter()
            .take(limit)
            .map(|(name, count)| format!("{}({})", name, count))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Local hour at which the daily run fires.
fn rsi_hour() -> u32 {
    std::env::var("JCWORK_RSI_HOUR")
        .ok()
        .and_then(|h| h.trim().parse::<u32>().ok())
        .filter(|h| *h <= 23)
        .unwrap_or(DEFAULT_RSI_HOUR)
}

/// Public view of the configured run hour, so the UI can say when the
/// daily analysis fires without duplicating the env lookup.
pub fn schedule_hour() -> u32 {
    rsi_hour()
}

/// Parse a report timestamp. Rows written by this service are RFC 3339;
/// `datetime('now')` fallbacks are UTC without a zone suffix.
fn parse_report_time(raw: &str) -> Option<DateTime<Utc>> {
    if let Ok(t) = DateTime::parse_from_rfc3339(raw) {
        return Some(t.with_timezone(&Utc));
    }
    chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S")
        .ok()
        .and_then(|t| Utc.from_local_datetime(&t).single())
}

/// Read the tail of a log file, dropping a possibly truncated first line.
async fn read_tail(path: &Path, max_bytes: u64) -> Option<String> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};

    let mut file = tokio::fs::File::open(path).await.ok()?;
    let len = file.metadata().await.ok()?.len();
    let truncated = len > max_bytes;
    if truncated {
        file.seek(std::io::SeekFrom::Start(len - max_bytes)).await.ok()?;
    }
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).await.ok()?;
    let mut text = String::from_utf8_lossy(&buf).to_string();
    if truncated {
        // The seek may land mid-line (or mid-character) — skip to the next
        // newline so the first parsed entry is complete.
        if let Some(pos) = text.find('\n') {
            text.drain(..=pos);
        }
    }
    Some(text)
}

/// Aggregate the deployment's log files over `[since, now]` per account.
pub async fn collect_usage(data_dir: &str, since: DateTime<Utc>) -> HashMap<String, UsageAgg> {
    let log_dir = PathBuf::from(data_dir).join("logs");
    let mut usage: HashMap<String, UsageAgg> = HashMap::new();

    let mut dates: Vec<String> = Vec::new();
    let mut day = since.date_naive();
    let today = Utc::now().date_naive();
    while day <= today && dates.len() < 4 {
        dates.push(day.format("%Y-%m-%d").to_string());
        let Some(next) = day.succ_opt() else { break };
        day = next;
    }

    let Ok(mut entries) = tokio::fs::read_dir(&log_dir).await else {
        return usage;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(".jsonl") || !dates.iter().any(|d| name.starts_with(d.as_str())) {
            continue;
        }
        let Some(text) = read_tail(&entry.path(), MAX_LOG_BYTES).await else {
            continue;
        };
        for line in text.lines() {
            let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            let Some(user_id) = value.get("user_id").and_then(|u| u.as_str()) else {
                continue;
            };
            usage
                .entry(user_id.to_string())
                .or_default()
                .absorb(line, since);
        }
    }

    usage
}

/// Render one usage line for the prompt.
fn usage_line(
    user: &User,
    agg: &UsageAgg,
    cron_success: i64,
    cron_error: i64,
    plans: usize,
) -> String {
    let role = if user.is_admin { "admin" } else { "user" };
    let visibility = if user.is_public { "public" } else { "private" };
    format!(
        "- {} ({} / {}): LLM请求={}, tokens={}, 工具调用={}（失败{}）, 周期任务={}（成功{} / 失败{}）, 主要工具={}",
        user.username,
        role,
        visibility,
        agg.llm_requests,
        agg.total_tokens,
        agg.tool_calls,
        agg.tool_errors,
        plans,
        cron_success,
        cron_error,
        agg.top_tools(5),
    )
}

/// Compact `path` listing for the prompt, bounded by entry count and chars.
fn render_tree(root: &Path, depth: usize, max_entries: usize, max_chars: usize) -> String {
    fn walk(dir: &Path, root: &Path, depth: usize, out: &mut Vec<String>, max_entries: usize) {
        if depth == 0 || out.len() >= max_entries {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut paths: Vec<(bool, PathBuf)> = entries
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let path = e.path();
                let name = e.file_name().to_string_lossy().to_string();
                if matches!(
                    name.as_str(),
                    ".git" | "node_modules" | "target" | "__pycache__" | "dist" | ".next"
                ) {
                    return None;
                }
                Some((path.is_dir(), path))
            })
            .collect();
        paths.sort_by(|a, b| a.1.cmp(&b.1));
        for (is_dir, path) in paths {
            if out.len() >= max_entries {
                return;
            }
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .to_string();
            out.push(if is_dir { format!("{}/", rel) } else { rel });
            if is_dir {
                walk(&path, root, depth - 1, out, max_entries);
            }
        }
    }

    let mut out = Vec::new();
    walk(root, root, depth, &mut out, max_entries);
    let mut text = out.join("\n");
    if text.chars().count() > max_chars {
        text = text.chars().take(max_chars).collect::<String>() + "\n…";
    }
    if text.trim().is_empty() {
        "（空）".to_string()
    } else {
        text
    }
}

/// Locate the project checkout for the admin proposal prompt.
///
/// The desktop bundle ships without sources, so this is best-effort: without
/// a checkout the prompt simply lacks the structural snapshot. The search
/// starts at `JCWORK_PROJECT_ROOT` (or the current directory) and walks up so
/// a server started from a subdirectory still finds the repository root.
fn project_root() -> Option<PathBuf> {
    let start = std::env::var("JCWORK_PROJECT_ROOT")
        .ok()
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())?;
    start
        .ancestors()
        .find(|dir| dir.join("Cargo.toml").is_file() && dir.join("crates").is_dir())
        .map(PathBuf::from)
}

/// The checkout offered to the agent as `source/` for read-only inspection.
pub(crate) fn source_root_for_admin() -> Option<PathBuf> {
    project_root()
}

fn project_tree_snapshot(root: Option<&Path>) -> String {
    let Some(root) = root else {
        return "（未找到项目源码目录：本次无法读取 source/。可在“自我改进”页配置源码目录，或设置 JCWORK_PROJECT_ROOT 指向仓库根目录后重试；本次请只依据使用数据给出方向性提案并注明验证方式）".to_string();
    };
    let mut parts = Vec::new();
    for sub in ["crates", "web/src"] {
        let dir = root.join(sub);
        if dir.is_dir() {
            parts.push(format!("{}:\n{}", sub, render_tree(&dir, 3, 220, PROJECT_TREE_MAX / 2)));
        }
    }
    if parts.is_empty() {
        "（项目结构不可读）".to_string()
    } else {
        parts.join("\n\n")
    }
}

/// Build the admin prompt: deployment-wide usage → project-code proposals.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_admin_prompt(
    users: &[User],
    usage: &HashMap<String, UsageAgg>,
    cron: &HashMap<String, (i64, i64)>,
    plan_counts: &HashMap<String, usize>,
    since: DateTime<Utc>,
    now: DateTime<Utc>,
    date: &str,
    source_root: Option<&Path>,
) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut ordered: Vec<(&User, UsageAgg)> = users
        .iter()
        .map(|u| (u, usage.get(&u.id).cloned().unwrap_or_default()))
        .collect();
    ordered.sort_by(|a, b| {
        b.1.llm_requests
            .cmp(&a.1.llm_requests)
            .then(a.0.username.cmp(&b.0.username))
    });
    for (user, agg) in &ordered {
        let (success, error) = cron.get(&user.id).copied().unwrap_or((0, 0));
        lines.push(usage_line(
            user,
            agg,
            success,
            error,
            plan_counts.get(&user.id).copied().unwrap_or(0),
        ));
    }

    format!(
        r#"你是 Jcowork 平台的 RSI（Recursive Self-Improvement，递归自我改进）分析器，当前以管理员（admin）身份运行。
目标：依据部署内各账户最近 24 小时的真实使用情况，阅读真实源码，给出可执行的 **项目源码改进计划**（Rust 后端 crates/、React 前端 web/）。

## 使用数据
窗口：{since} → {now}（UTC）
账户数：{user_count}
{usage}

## 项目结构快照
{tree}

## 源码目录（只读）
- 当前源码根目录：{source_path}
- 源码根目录以 `source/` 前缀访问，例如 `source/crates/jcowork-gateway/src/rsi.rs`、`source/web/src/App.tsx`。
- 先用 file_search 在 `source/` 内定位关键字，再用 file_read 阅读具体实现（大文件用 offset/limit 分片读取）。
- `source/` 只读：写入会被拒绝；需要写内容时只能写到你自己的工作目录。

## 工作方式
1. 先识别异常与改进点：失败率高的工具、重复或无效的请求模式、token 消耗异常、周期任务失败、长期未使用的账户/功能、管理员与普通用户的使用差异。
2. 至少阅读 2-4 个与提案直接相关的源码文件（从 `source/` 读取），提案必须基于真实代码，不得凭空猜测。
3. 每条提案都要引用上面的具体使用数据作为证据，并落到具体文件、函数/结构体。
4. 不要修改项目源码：用 file_write 把完整提案写入 `rsi/proposals/{date}.md`（目录不存在时会自动创建）。

## 最终回复（中文，不超过 800 字）
按优先级给出 3-5 条源码改进计划，每条包含：
- 问题（引用使用数据）
- 证据（源码文件 + 关键函数/结构体）
- 改动方案（具体到文件/函数，分步骤）
- 验证方式（测试或检查点）
- 预期收益
最后补充一句「本次分析的数据缺口」（如果没有则写「无」）。"#,
        since = since.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        now = now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        user_count = users.len(),
        usage = if lines.is_empty() { "（窗口内无使用记录）".to_string() } else { lines.join("\n") },
        tree = project_tree_snapshot(source_root),
        source_path = source_root
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "（未配置：本次无法读取 source/）".to_string()),
        date = date,
    )
}

/// Build the normal-user prompt: own usage + workspace + plans → workspace
/// improvements and plan adjustments.
pub(crate) fn build_user_prompt(
    usage: &UsageAgg,
    cron_success: i64,
    cron_error: i64,
    plans: &[(String, String, String)],
    workspace: &str,
    since: DateTime<Utc>,
    now: DateTime<Utc>,
    date: &str,
) -> String {
    let plans_text = if plans.is_empty() {
        "（当前没有周期计划）".to_string()
    } else {
        plans
            .iter()
            .map(|(id, schedule, prompt)| {
                let brief: String = prompt.chars().take(160).collect();
                format!("- {} | {} | {}", id, schedule, brief)
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    format!(
        r#"你是 Jcowork 的 RSI（递归自我改进）助手，当前以普通用户身份运行。
目标：依据该账户最近 24 小时的使用情况，自我改进两类对象：
A. 工作目录下的流程与代码（流程文档、脚本、模板等）；
B. 周期计划（定时任务）。

## 使用数据
窗口：{since} → {now}（UTC）
LLM请求={requests}, tokens={tokens}, 工具调用={tool_calls}（失败{tool_errors}）, 主要工具={top_tools}
周期任务执行：成功{cron_success} / 失败{cron_error}

## 工作目录文件（最多两层）
{workspace}

## 当前周期计划
{plans}

## 工作方式
1. 先用 file_read 阅读与本次改进最相关的 1-3 个文件，避免凭空改写。
2. 流程/代码改进：优先更新已有文件；需要新建时放在 `rsi/` 目录（如 `rsi/playbook.md`）。改动保持最小、可回滚，不要删除用户数据。
3. 周期计划改进：只在确有收益时调整。最多新增 1 个、删除 1 个重复或失效的计划；新增计划的提示词要具体可执行，并说明运行时间，不得与现有计划重复。
4. 用 file_write 把本次改进记录写入 `rsi/reports/{date}.md`（包含：改了什么、为什么、如何回滚）。
5. 除 `rsi/` 目录和确实需要修正的流程文件外，不要改动无关文件。

## 最终回复（中文，不超过 500 字）
- 改进了什么（文件 / 周期计划，逐条）
- 依据（引用上面的使用数据）
- 建议用户下一步做什么"#,
        since = since.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        now = now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        requests = usage.llm_requests,
        tokens = usage.total_tokens,
        tool_calls = usage.tool_calls,
        tool_errors = usage.tool_errors,
        top_tools = usage.top_tools(6),
        cron_success = cron_success,
        cron_error = cron_error,
        workspace = workspace,
        plans = plans_text,
        date = date,
    )
}

/// First meaningful line of the agent answer, as a list teaser.
pub(crate) fn make_summary(output: &str) -> String {
    let line = output
        .lines()
        .map(|l| l.trim().trim_start_matches('#').trim())
        .find(|l| !l.is_empty())
        .unwrap_or("（无输出）");
    let chars: Vec<char> = line.chars().collect();
    if chars.len() > 160 {
        let cut: String = chars.into_iter().take(160).collect();
        format!("{}…", cut)
    } else {
        line.to_string()
    }
}

/// Captures the agent answer and error for a background run.
struct RsiSink {
    response: String,
    error: Option<String>,
}

impl RsiSink {
    fn new() -> Self {
        Self {
            response: String::new(),
            error: None,
        }
    }
}

impl AgentOutputSink for RsiSink {
    fn on_text_delta<'b>(
        &'b mut self,
        text: &'b str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'b>> {
        self.response.push_str(text);
        Box::pin(async {})
    }
    fn on_tool_call_start<'b>(
        &'b mut self,
        _name: &'b str,
        _call_id: &'b str,
        _arguments: &'b str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'b>> {
        Box::pin(async {})
    }
    fn on_tool_call_end<'b>(
        &'b mut self,
        _name: &'b str,
        _result: &'b str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'b>> {
        Box::pin(async {})
    }
    fn on_done<'b>(
        &'b mut self,
        _usage: Option<(i32, i32, i32)>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'b>> {
        Box::pin(async {})
    }
    fn on_error<'b>(
        &'b mut self,
        message: &'b str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'b>> {
        self.error = Some(message.to_string());
        Box::pin(async {})
    }
    fn on_status<'b>(
        &'b mut self,
        _message: &'b str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'b>> {
        Box::pin(async {})
    }
}

/// Runs RSI analyses and hosts the daily scheduler.
pub struct RsiService {
    llm_router: Arc<RwLock<LlmRouter>>,
    default_model: String,
    tool_registry: Arc<ToolRegistry>,
    log_writer: Arc<LogWriter>,
    data_dir: String,
    user_store: Arc<UserStore>,
    user_settings: Arc<UserSettingsStore>,
    store: Arc<RsiStore>,
    cron_scheduler: Arc<CronScheduler>,
    /// Deployment-level settings (the admin-configured source checkout).
    app_settings: Arc<AppSettingsStore>,
    /// Accounts with a run in flight, so a manual "run now" cannot overlap
    /// the scheduler's run for the same account.
    running: DashSet<String>,
}

impl RsiService {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        llm_router: Arc<RwLock<LlmRouter>>,
        default_model: String,
        tool_registry: Arc<ToolRegistry>,
        log_writer: Arc<LogWriter>,
        data_dir: String,
        user_store: Arc<UserStore>,
        user_settings: Arc<UserSettingsStore>,
        store: Arc<RsiStore>,
        cron_scheduler: Arc<CronScheduler>,
        app_settings: Arc<AppSettingsStore>,
    ) -> Self {
        Self {
            llm_router,
            default_model,
            tool_registry,
            log_writer,
            data_dir,
            user_store,
            user_settings,
            store,
            cron_scheduler,
            app_settings,
            running: DashSet::new(),
        }
    }

    pub fn is_running(&self, user_id: &str) -> bool {
        self.running.contains(user_id)
    }

    /// The checkout an admin run may inspect, read-only.
    ///
    /// The configured directory wins when it still exists; a stale setting
    /// falls back to auto-detection rather than disabling the run. `None`
    /// means no `source/` tree for this run.
    pub async fn source_root(&self) -> Option<PathBuf> {
        match self.app_settings.get(SOURCE_SETTINGS_KEY).await {
            Ok(Some(raw)) => {
                let trimmed = raw.trim();
                if !trimmed.is_empty() {
                    let path = PathBuf::from(trimmed);
                    if path.is_dir() {
                        return Some(path);
                    }
                    tracing::warn!(
                        path = %trimmed,
                        "Configured RSI source directory is missing; falling back to auto-detection"
                    );
                }
            }
            Ok(None) => {}
            Err(e) => tracing::warn!(error = %e, "Stored RSI source setting unreadable"),
        }
        source_root_for_admin()
    }

    /// The configured override (`None` = auto-detect) and the path a run
    /// would actually use. Both are exposed by `GET /api/rsi`.
    pub async fn source_config(&self) -> (Option<String>, Option<String>) {
        let configured = self
            .app_settings
            .get(SOURCE_SETTINGS_KEY)
            .await
            .ok()
            .flatten()
            .map(|raw| raw.trim().to_string())
            .filter(|raw| !raw.is_empty());
        let resolved = self
            .source_root()
            .await
            .map(|p| p.to_string_lossy().to_string());
        (configured, resolved)
    }

    /// Whether the account opted in.
    pub async fn enabled(&self, user_id: &str) -> bool {
        self.user_settings
            .get_rsi_enabled(user_id)
            .await
            .unwrap_or(false)
    }

    pub async fn set_enabled(&self, user_id: &str, enabled: bool) -> Result<()> {
        self.user_settings.set_rsi_enabled(user_id, enabled).await
    }

    pub async fn last_report_at(&self, user_id: &str) -> Option<String> {
        self.store.latest_report_at(user_id).await.ok().flatten()
    }

    /// Newest reports for the account, without their full output.
    pub async fn list_reports(
        &self,
        user_id: &str,
        limit: i64,
    ) -> Result<Vec<jcowork_storage::rsi_store::RsiReportSummary>> {
        self.store.list_reports(user_id, limit).await
    }

    /// One report, scoped to its owner.
    pub async fn get_report(
        &self,
        user_id: &str,
        id: &str,
    ) -> Result<Option<jcowork_storage::rsi_store::RsiReport>> {
        self.store.get_report(user_id, id).await
    }

    /// Start a run in the background. Returns false when a run for this
    /// account is already in flight.
    pub fn run_now(self: &Arc<Self>, user: User) -> bool {
        if !self.running.insert(user.id.clone()) {
            return false;
        }
        let service = Arc::clone(self);
        tokio::spawn(async move {
            let user_id = user.id.clone();
            let result = std::panic::AssertUnwindSafe(service.run_for_user(&user))
                .catch_unwind()
                .await;
            match result {
                Ok(Ok(report)) => {
                    tracing::info!(
                        user_id = %report.user_id,
                        role = %report.role,
                        status = %report.status,
                        "RSI run finished"
                    );
                }
                Ok(Err(e)) => tracing::warn!(user_id = %user_id, error = %e, "RSI run failed"),
                Err(_) => tracing::error!(user_id = %user_id, "RSI run panicked"),
            }
            service.running.remove(&user_id);
        });
        true
    }

    /// Spawn the daily scheduler loop.
    pub fn spawn_scheduler(self: Arc<Self>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            tracing::info!(hour = rsi_hour(), "RSI scheduler started");
            loop {
                if let Err(e) = self.tick().await {
                    tracing::warn!(error = %e, "RSI scheduler tick failed");
                }
                tokio::time::sleep(SCHEDULER_TICK).await;
            }
        })
    }

    /// One scheduler pass: run every opted-in account that has not run today
    /// once the configured local hour has passed.
    async fn tick(&self) -> Result<()> {
        let now_local = Local::now();
        if now_local.hour() < rsi_hour() {
            return Ok(());
        }
        let today = now_local.date_naive();

        for user_id in self.user_settings.list_rsi_enabled_user_ids().await? {
            if self.running.contains(&user_id) {
                continue;
            }
            // Already ran today (local date)?
            if let Some(raw) = self.store.latest_report_at(&user_id).await? {
                if let Some(at) = parse_report_time(&raw) {
                    if at.with_timezone(&Local).date_naive() == today {
                        continue;
                    }
                }
            }
            let user = match self.user_store.get_user_by_id(&user_id).await? {
                Some(u) if u.deleted_at.is_none() => u,
                _ => continue,
            };
            if !self.running.insert(user_id.clone()) {
                continue;
            }
            tracing::info!(user_id = %user_id, "RSI scheduler starting daily run");
            let result = std::panic::AssertUnwindSafe(self.run_for_user(&user))
                .catch_unwind()
                .await;
            match result {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => tracing::warn!(user_id = %user_id, error = %e, "RSI daily run failed"),
                Err(_) => tracing::error!(user_id = %user_id, "RSI daily run panicked"),
            }
            self.running.remove(&user_id);
        }
        Ok(())
    }

    /// Execute one analysis synchronously and persist its report.
    pub async fn run_for_user(&self, user: &User) -> Result<RsiReport> {
        let now = Utc::now();
        let since = now - chrono::Duration::hours(WINDOW_HOURS);
        let date = Local::now().format("%Y-%m-%d").to_string();
        let is_admin = user.is_admin;
        // Resolve once so the prompt's tree snapshot and the `source/` tool
        // root always point at the same checkout.
        let source_root = if is_admin { self.source_root().await } else { None };

        let model = self.resolve_model(user).await;
        let provider = {
            let router = self
                .llm_router
                .read()
                .map_err(|e| anyhow::anyhow!("Router lock poisoned: {}", e))?;
            router.get_provider(&model)?
        };

        let usage_map = collect_usage(&self.data_dir, since).await;
        let cron_stats = self
            .store
            .cron_task_stats_since(&since.to_rfc3339())
            .await
            .unwrap_or_default();
        let mut cron_by_user: HashMap<String, (i64, i64)> = HashMap::new();
        for stat in &cron_stats {
            let entry = cron_by_user.entry(stat.user_id.clone()).or_insert((0, 0));
            match stat.status.as_str() {
                "success" => entry.0 += stat.count,
                _ => entry.1 += stat.count,
            }
        }

        let (prompt, tool_names) = if is_admin {
            let users = self.user_store.list_users(None, "active").await?;
            let mut plan_counts: HashMap<String, usize> = HashMap::new();
            for u in &users {
                plan_counts.insert(u.id.clone(), self.cron_scheduler.list_cron_jobs(&u.id).await.len());
            }
            (
                build_admin_prompt(
                    &users,
                    &usage_map,
                    &cron_by_user,
                    &plan_counts,
                    since,
                    now,
                    &date,
                    source_root.as_deref(),
                ),
                ADMIN_TOOLS,
            )
        } else {
            let own = usage_map.get(&user.id).cloned().unwrap_or_default();
            let (success, error) = cron_by_user.get(&user.id).copied().unwrap_or((0, 0));
            let jobs = self.cron_scheduler.list_cron_jobs(&user.id).await;
            let plans: Vec<(String, String, String)> = jobs
                .iter()
                .map(|j| (j.id.clone(), j.schedule.clone(), j.prompt.clone()))
                .collect();
            let workspace_root = format!("{}/{}/workspace", self.data_dir, user.id);
            let tree = render_tree(&PathBuf::from(&workspace_root), 2, 80, WORKSPACE_TREE_MAX);
            (
                build_user_prompt(&own, success, error, &plans, &tree, since, now, &date),
                USER_TOOLS,
            )
        };

        let allowed: HashSet<&str> = tool_names.iter().copied().collect();
        let tools: Vec<_> = self
            .tool_registry
            .all_schemas()
            .into_iter()
            .filter(|t| allowed.contains(t.function.name.as_str()))
            .collect();

        let workspace_root = format!("{}/{}/workspace", self.data_dir, user.id);
        let _ = tokio::fs::create_dir_all(&workspace_root).await;
        // Admin runs may inspect the checkout through the `source/` prefix
        // (read-only — file writes into it are rejected by the tools).
        let source_root = source_root.map(|p| p.to_string_lossy().to_string());
        let tool_ctx = ToolContext {
            user_id: user.id.clone(),
            workspace_root: workspace_root.clone(),
            source_root,
            mentioned_public_users: Vec::new(),
            model: Some(model.clone()),
        };

        let system_prompt = format!(
            "你是 Jcowork 的 RSI（递归自我改进）执行器，当前账户角色：{}。\
             你只做有据可依、最小化、可回滚的改进；每项改动都要在最终回复中说明依据。",
            if is_admin { "admin（部署管理员）" } else { "user（普通用户）" }
        );
        let mut history = vec![
            jcowork_llm::provider::ChatMessage {
                role: "system".to_string(),
                content: system_prompt,
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
                images: None,
            },
            jcowork_llm::provider::ChatMessage {
                role: "user".to_string(),
                content: prompt,
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
                images: None,
            },
        ];

        let mut sink = RsiSink::new();
        let result = agent_loop::run_turn(AgentTurnOptions {
            history: &mut history,
            tools: &tools,
            provider,
            tool_registry: self.tool_registry.clone(),
            tool_ctx: &tool_ctx,
            pre_context: None,
            max_turns: MAX_TURNS,
            llm_timeout_secs: 180,
            stream_timeout_secs: 180,
            tool_timeout_secs: 90,
            output: &mut sink,
            user_id: &user.id,
            model: &model,
            log_writer: Some(self.log_writer.clone()),
        })
        .await;

        let sink_error = sink.error.take();
        let (output, status) = if result.completed && !result.response.is_empty() {
            (result.response, "success")
        } else if let Some(err) = sink_error {
            (err, "error")
        } else if !result.completed {
            (
                format!(
                    "RSI 在 {} 轮内没有给出结论，本次未产生改进建议。",
                    result.turns_used
                ),
                "error",
            )
        } else {
            ("模型返回了空响应。".to_string(), "error")
        };

        // Mirror the answer into the workspace so it also appears in
        // Documents; the database row stays the canonical record.
        if status == "success" {
            if let Err(e) = write_report_file(&self.data_dir, user, &date, &output).await {
                tracing::warn!(user_id = %user.id, error = %e, "Failed to write RSI report file");
            }
        }

        let report = RsiReport {
            id: uuid::Uuid::new_v4().to_string(),
            user_id: user.id.clone(),
            role: if is_admin { "admin".to_string() } else { "user".to_string() },
            period_start: since.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            period_end: now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            summary: make_summary(&output),
            output,
            status: status.to_string(),
            created_at: Utc::now().to_rfc3339(),
        };
        self.store.insert_report(&report).await?;
        Ok(report)
    }

    /// The model the account chats with, else the deployment default. A
    /// saved model whose provider has since been removed must not break the
    /// daily run.
    async fn resolve_model(&self, user: &User) -> String {
        let saved = self.user_settings.get_model(&user.id).await.ok().flatten();
        if let (Some(saved), Ok(router)) = (saved.as_deref(), self.llm_router.read()) {
            if router.get_provider(saved).is_ok() {
                return saved.to_string();
            }
        }
        self.default_model.clone()
    }
}

/// Write the run's markdown into `rsi/proposals/` (admin) or `rsi/reports/`
/// (user) and index it so it shows up in Documents.
async fn write_report_file(
    data_dir: &str,
    user: &User,
    date: &str,
    output: &str,
) -> Result<()> {
    let sub = if user.is_admin { "proposals" } else { "reports" };
    let rel = format!("rsi/{}/{}.md", sub, date);
    let workspace_root = format!("{}/{}/workspace", data_dir, user.id);
    let full = PathBuf::from(&workspace_root).join(&rel);
    if let Some(parent) = full.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let title = if user.is_admin {
        "RSI 项目代码改进提案"
    } else {
        "RSI 自我改进报告"
    };
    let body = format!(
        "# {} {}\n\n> 账户：{} | 生成时间：{}\n\n{}\n",
        title,
        date,
        user.username,
        Local::now().format("%Y-%m-%d %H:%M:%S"),
        output
    );
    tokio::fs::write(&full, body).await?;

    if let Ok(index) = jcowork_storage::WorkspaceIndex::cached(data_dir, &user.id).await {
        if let Err(e) = index.add_document(&rel, &workspace_root).await {
            tracing::warn!(file = %rel, error = %e, "Failed to index RSI report");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn since() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-11-14T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn usage_absorbs_llm_requests_and_tool_calls() {
        let mut agg = UsageAgg::default();
        assert!(agg.absorb(
            r#"{"event":"llm_request","timestamp":"2026-11-14T10:00:00.000Z","user_id":"u1","output":{"usage":{"total_tokens":123}}}"#,
            since()
        ));
        assert!(agg.absorb(
            r#"{"event":"tool_call","timestamp":"2026-11-14T10:01:00.000Z","user_id":"u1","tool":"file_read","output":"ok"}"#,
            since()
        ));
        assert!(agg.absorb(
            r#"{"event":"tool_call","timestamp":"2026-11-14T10:02:00.000Z","user_id":"u1","tool":"file_read","output":"Error: missing file"}"#,
            since()
        ));
        assert_eq!(agg.llm_requests, 1);
        assert_eq!(agg.total_tokens, 123);
        assert_eq!(agg.tool_calls, 2);
        assert_eq!(agg.tool_errors, 1);
        assert_eq!(agg.tools.get("file_read"), Some(&2));
        assert_eq!(agg.top_tools(5), "file_read(2)");
    }

    #[test]
    fn usage_ignores_lines_outside_window_and_malformed() {
        let mut agg = UsageAgg::default();
        assert!(!agg.absorb("not json", since()));
        assert!(!agg.absorb(
            r#"{"event":"llm_request","timestamp":"2026-11-13T23:59:59.000Z","user_id":"u1"}"#,
            since()
        ));
        assert!(!agg.absorb(
            r#"{"event":"raw_data","timestamp":"2026-11-14T10:00:00.000Z","user_id":"u1"}"#,
            since()
        ));
        assert_eq!(agg.llm_requests, 0);
        assert_eq!(agg.tool_calls, 0);
    }

    #[test]
    fn summary_uses_first_meaningful_line() {
        assert_eq!(make_summary("\n\n## 改进提案\n详情"), "改进提案");
        assert_eq!(make_summary(""), "（无输出）");
        let long = "长".repeat(200);
        let summary = make_summary(&long);
        assert_eq!(summary.chars().count(), 161, "160 chars plus the ellipsis");
        assert!(summary.ends_with('…'));
    }

    fn user(name: &str, admin: bool, public: bool) -> User {
        User {
            id: format!("id-{}", name),
            username: name.to_string(),
            password_hash: String::new(),
            feishu_open_id: None,
            phone: None,
            email: None,
            created_at: String::new(),
            is_public: public,
            is_admin: admin,
            deleted_at: None,
            password_changed_at: None,
        }
    }

    #[test]
    fn admin_prompt_lists_every_account_with_evidence() {
        let users = vec![user("admin", true, false), user("alice", false, true)];
        let mut usage = HashMap::new();
        let mut agg = UsageAgg::default();
        agg.absorb(
            r#"{"event":"llm_request","timestamp":"2026-11-14T10:00:00.000Z","user_id":"id-alice","output":{"usage":{"total_tokens":500}}}"#,
            since(),
        );
        usage.insert("id-alice".to_string(), agg);
        let mut cron = HashMap::new();
        cron.insert("id-alice".to_string(), (3, 1));
        let mut plans = HashMap::new();
        plans.insert("id-alice".to_string(), 2usize);

        let now = DateTime::parse_from_rfc3339("2026-11-15T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let prompt = build_admin_prompt(
            &users,
            &usage,
            &cron,
            &plans,
            since(),
            now,
            "2026-11-15",
            Some(Path::new("/checkout")),
        );

        assert!(prompt.contains("alice (user / public)"));
        assert!(prompt.contains("tokens=500"));
        assert!(prompt.contains("周期任务=2（成功3 / 失败1）"));
        assert!(prompt.contains("admin (admin / private)"));
        assert!(prompt.contains("rsi/proposals/2026-11-15.md"));
        // The admin must be told the checkout is readable via `source/` and
        // that the deliverable is a source-level plan with verification.
        assert!(prompt.contains("`source/` 前缀"));
        assert!(prompt.contains("源码改进计划"));
        assert!(prompt.contains("验证方式"));
        // The resolved checkout is stated explicitly, so the model knows
        // which tree the tree snapshot came from.
        assert!(prompt.contains("当前源码根目录：/checkout"));
        assert!(!prompt.contains("file_delete"));
    }

    #[test]
    fn admin_prompt_states_a_missing_checkout() {
        let users = vec![user("admin", true, false)];
        let now = DateTime::parse_from_rfc3339("2026-11-15T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let prompt = build_admin_prompt(
            &users,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            since(),
            now,
            "2026-11-15",
            None,
        );
        assert!(prompt.contains("当前源码根目录：（未配置：本次无法读取 source/）"));
        assert!(prompt.contains("可在“自我改进”页配置源码目录"));
    }

    #[test]
    fn user_prompt_contains_plans_and_report_path() {
        let plans = vec![(
            "job-1".to_string(),
            "0 9 * * *".to_string(),
            "每天早上整理收件箱".to_string(),
        )];
        let now = DateTime::parse_from_rfc3339("2026-11-15T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let prompt = build_user_prompt(
            &UsageAgg::default(),
            2,
            0,
            &plans,
            "notes/\nnotes/todo.md",
            since(),
            now,
            "2026-11-15",
        );
        assert!(prompt.contains("job-1 | 0 9 * * * | 每天早上整理收件箱"));
        assert!(prompt.contains("周期任务执行：成功2 / 失败0"));
        assert!(prompt.contains("notes/todo.md"));
        assert!(prompt.contains("rsi/reports/2026-11-15.md"));
        assert!(prompt.contains("最多新增 1 个"));
    }

    #[test]
    fn user_prompt_handles_no_plans() {
        let now = DateTime::parse_from_rfc3339("2026-11-15T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let prompt = build_user_prompt(
            &UsageAgg::default(),
            0,
            0,
            &[],
            "（空）",
            since(),
            now,
            "2026-11-15",
        );
        assert!(prompt.contains("（当前没有周期计划）"));
    }

    #[test]
    fn parse_report_time_accepts_both_formats() {
        assert!(parse_report_time("2026-11-15T03:00:00+00:00").is_some());
        assert!(parse_report_time("2026-11-15 03:00:00").is_some());
        assert!(parse_report_time("garbage").is_none());
    }
}
