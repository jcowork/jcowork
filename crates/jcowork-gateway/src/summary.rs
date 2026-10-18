//! Background AI teaser summaries for the aggregated feed.
//!
//! Task results and indexed documents each get a short LLM-written teaser
//! stored next to them, so feed cards show a punchy hook instead of a raw
//! content truncation. Generation is fire-and-forget: the write paths and
//! the feed reader call the `ensure_*` methods, an in-flight set dedups
//! concurrent attempts, and any failure simply leaves the raw truncation
//! in place — the next feed view retries.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use tokio::sync::Semaphore;

use jcowork_cron::{CronScheduler, UserTaskResultEntry};
use jcowork_llm::LlmRouter;
use jcowork_llm::provider::ChatMessage;
use jcowork_storage::WorkspaceIndex;

/// At most this many summaries generate concurrently.
const MAX_CONCURRENT: usize = 3;
/// Hard timeout for one summary LLM call.
const LLM_TIMEOUT_SECS: u64 = 60;
/// How much of the source content is fed to the model.
const SOURCE_HEAD_CHARS: usize = 4000;
/// Content shorter than this already fits on the card — no summary needed.
const MIN_SOURCE_CHARS: usize = 200;
/// Hard cap on the stored summary length (the prompt asks for less).
const MAX_SUMMARY_CHARS: usize = 120;

const SYSTEM_PROMPT: &str = "你是「AI头条」信息流的编辑,任务是给一段内容写一句让人想点进去看完整的摘要。规则:
- 用内容本身的语言写(中文内容写中文,英文内容写英文,以此类推)
- 一到两句话,不超过 60 个汉字(英文内容不超过 120 字符)
- 第一句就给出内容里最有价值、最具体的信息点或结论,不写空洞铺垫
- 可以制造合理的好奇心,但严禁编造内容中不存在的信息,不用无实质的夸张词
- 只输出摘要正文:不要标题、不要引号、不要 Markdown、不要「摘要:」之类前缀";

pub struct SummaryService {
    llm_router: Arc<RwLock<LlmRouter>>,
    default_model: String,
    /// Keys of summaries currently generating ("task:{id}" / "doc:{user}:{path}").
    inflight: Mutex<HashSet<String>>,
    sem: Arc<Semaphore>,
}

impl SummaryService {
    pub fn new(llm_router: Arc<RwLock<LlmRouter>>, default_model: String) -> Self {
        Self {
            llm_router,
            default_model,
            inflight: Mutex::new(HashSet::new()),
            sem: Arc::new(Semaphore::new(MAX_CONCURRENT)),
        }
    }

    /// Fire-and-forget: generate and persist a teaser for a task result that
    /// does not have one yet. No-op when a generation is already running.
    pub fn ensure_task_summary(
        self: &Arc<Self>,
        cron_scheduler: Arc<CronScheduler>,
        result_id: &str,
    ) {
        let key = format!("task:{}", result_id);
        if !self.try_begin(&key) {
            return;
        }
        let svc = Arc::clone(self);
        let result_id = result_id.to_string();
        tokio::spawn(async move {
            let _permit = svc.acquire_slot().await;
            generate_task_summary(&svc, &cron_scheduler, &result_id).await;
            svc.end(&key);
        });
    }

    /// Fire-and-forget: generate and persist a teaser for an indexed document
    /// that does not have one yet. No-op when a generation is already running.
    pub fn ensure_doc_summary(self: &Arc<Self>, data_dir: &str, user_id: &str, file_path: &str) {
        let key = format!("doc:{}:{}", user_id, file_path);
        if !self.try_begin(&key) {
            return;
        }
        let svc = Arc::clone(self);
        let data_dir = data_dir.to_string();
        let user_id = user_id.to_string();
        let file_path = file_path.to_string();
        tokio::spawn(async move {
            let _permit = svc.acquire_slot().await;
            generate_doc_summary(&svc, &data_dir, &user_id, &file_path).await;
            svc.end(&key);
        });
    }

    async fn acquire_slot(&self) -> tokio::sync::OwnedSemaphorePermit {
        self.sem
            .clone()
            .acquire_owned()
            .await
            .expect("summary semaphore is never closed")
    }

    fn try_begin(&self, key: &str) -> bool {
        self.inflight
            .lock()
            .expect("summary inflight lock poisoned")
            .insert(key.to_string())
    }

    fn end(&self, key: &str) {
        self.inflight
            .lock()
            .expect("summary inflight lock poisoned")
            .remove(key);
    }

    /// One non-streaming LLM call through the default model.
    /// Returns None on any failure (already logged); the caller keeps the
    /// raw truncation as the card text.
    async fn generate(&self, user_message: &str) -> Option<String> {
        let provider = {
            let router = self.llm_router.read().ok()?;
            match router.get_provider(&self.default_model) {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        model = %self.default_model,
                        "Summary generation: provider unavailable"
                    );
                    return None;
                }
            }
        };
        let messages = [
            ChatMessage {
                role: "system".to_string(),
                content: SYSTEM_PROMPT.to_string(),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
                images: None,
            },
            ChatMessage {
                role: "user".to_string(),
                content: user_message.to_string(),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
                images: None,
            },
        ];
        match tokio::time::timeout(
            Duration::from_secs(LLM_TIMEOUT_SECS),
            provider.chat(&messages, &[]),
        )
        .await
        {
            Ok(Ok(resp)) => {
                let summary = sanitize_summary(&resp.message.content);
                if summary.is_empty() {
                    tracing::warn!("Summary generation: model returned no usable text");
                    None
                } else {
                    Some(summary)
                }
            }
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "Summary generation: LLM call failed");
                None
            }
            Err(_) => {
                tracing::warn!(secs = LLM_TIMEOUT_SECS, "Summary generation: LLM call timed out");
                None
            }
        }
    }
}

async fn generate_task_summary(
    svc: &SummaryService,
    cron_scheduler: &CronScheduler,
    result_id: &str,
) {
    let entry = match cron_scheduler.get_task_result_by_id(result_id).await {
        Some(e) => e,
        None => return, // deleted before the job ran — nothing to do
    };
    if !should_summarize_task(&entry) {
        return; // already summarized, failed, or short enough to show as-is
    }
    let user_message = build_task_message(
        entry.job_name.as_deref(),
        entry.job_prompt.as_deref(),
        &entry.output,
    );
    if let Some(summary) = svc.generate(&user_message).await {
        cron_scheduler.set_task_result_summary(result_id, &summary).await;
        tracing::info!(result_id = %result_id, len = summary.chars().count(), "Task result summary stored");
    }
}

async fn generate_doc_summary(svc: &SummaryService, data_dir: &str, user_id: &str, file_path: &str) {
    let index = match WorkspaceIndex::cached(data_dir, user_id).await {
        Ok(i) => i,
        Err(e) => {
            tracing::warn!(error = %e, user_id = %user_id, "Summary generation: index open failed");
            return;
        }
    };
    let source = match index.get_summary_source(file_path).await {
        Ok(Some(s)) => s,
        Ok(None) => return, // document no longer indexed
        Err(e) => {
            tracing::warn!(error = %e, file = %file_path, "Summary generation: source read failed");
            return;
        }
    };
    if !source.summary.is_empty() {
        return; // already summarized
    }
    if !should_summarize_doc(&source.head) {
        return; // placeholder text, or short enough to show as-is
    }
    let user_message = build_doc_message(file_path, &source.head);
    if let Some(summary) = svc.generate(&user_message).await {
        if let Err(e) = index.set_document_summary(file_path, &summary).await {
            tracing::warn!(error = %e, file = %file_path, "Summary generation: store failed");
        } else {
            tracing::info!(file = %file_path, len = summary.chars().count(), "Document summary stored");
        }
    }
}

/// A task result earns a summary only when it succeeded and its output is
/// long enough that the raw excerpt does not already tell the whole story.
fn should_summarize_task(entry: &UserTaskResultEntry) -> bool {
    entry.status == "success" && entry.output.trim().chars().count() >= MIN_SOURCE_CHARS
}

/// A document earns a summary only when its extracted text is real content
/// (not a parse-error placeholder) and long enough to hide anything.
fn should_summarize_doc(head: &str) -> bool {
    let t = head.trim();
    if t.chars().count() < MIN_SOURCE_CHARS {
        return false;
    }
    !t.starts_with("[Document parse") && !t.starts_with("[Excel import error")
}

pub(crate) fn build_task_message(
    job_name: Option<&str>,
    job_prompt: Option<&str>,
    output: &str,
) -> String {
    let mut m = String::from("【周期任务】\n");
    if let Some(name) = job_name.filter(|s| !s.trim().is_empty()) {
        m.push_str("任务名:");
        m.push_str(name.trim());
        m.push('\n');
    }
    if let Some(p) = job_prompt.filter(|s| !s.trim().is_empty()) {
        m.push_str("任务指令:");
        m.push_str(p.trim());
        m.push('\n');
    }
    m.push_str("执行结果(可能截断):\n");
    m.push_str(&head_chars(output, SOURCE_HEAD_CHARS));
    m
}

pub(crate) fn build_doc_message(file_path: &str, head: &str) -> String {
    let filename = file_path.rsplit('/').next().unwrap_or(file_path);
    format!("【文档】\n文件名:{}\n内容(可能截断):\n{}", filename, head)
}

fn head_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// Clean up a model answer into a one-line card teaser.
pub(crate) fn sanitize_summary(raw: &str) -> String {
    let mut s = raw.trim();

    // Strip a wrapping markdown code fence (```lang … ```).
    if let Some(rest) = s.strip_prefix("```") {
        if let Some(nl) = rest.find('\n') {
            let inner = rest[nl + 1..].trim_end();
            if let Some(inner) = inner.strip_suffix("```") {
                s = inner.trim();
            }
        }
    }

    // Strip a "Summary:"-style prefix the model was told not to emit.
    // Covers both the fullwidth and ASCII colons after 「摘要」.
    for p in ["\u{6458}\u{8981}\u{ff1a}", "\u{6458}\u{8981}:", "Summary:", "summary:"] {
        if let Some(rest) = s.strip_prefix(p) {
            s = rest.trim();
            break;
        }
    }

    // Strip paired wrapping quotes, repeatedly ("他说:「…」" → inner text).
    const PAIRS: [(char, char); 5] = [
        ('「', '」'),
        ('『', '』'),
        ('“', '”'),
        ('‘', '’'),
        ('"', '"'),
    ];
    let mut chars: Vec<char> = s.chars().collect();
    while chars.len() >= 2 {
        let first = chars[0];
        let last = *chars.last().expect("len checked above");
        if !PAIRS.iter().any(|(o, c)| first == *o && last == *c) {
            break;
        }
        chars.remove(0);
        chars.pop();
    }
    if chars.len() > MAX_SUMMARY_CHARS {
        chars.truncate(MAX_SUMMARY_CHARS);
    }

    chars
        .into_iter()
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_trims_and_collapses_whitespace() {
        assert_eq!(sanitize_summary("  三句话\n读懂\t今日要点  "), "三句话 读懂 今日要点");
        assert_eq!(sanitize_summary(""), "");
        assert_eq!(sanitize_summary("   "), "");
    }

    #[test]
    fn sanitize_strips_wrapping_quotes_and_fences() {
        assert_eq!(sanitize_summary("「模型放出了重磅结论」"), "模型放出了重磅结论");
        assert_eq!(sanitize_summary("“引用式摘要”"), "引用式摘要");
        assert_eq!(sanitize_summary("\"quoted\""), "quoted");
        assert_eq!(sanitize_summary("```\n摘要正文在这里\n```"), "摘要正文在这里");
    }

    #[test]
    fn sanitize_strips_summary_prefix() {
        assert_eq!(sanitize_summary("\u{6458}\u{8981}\u{ff1a}真正的要点"), "真正的要点");
        assert_eq!(sanitize_summary("\u{6458}\u{8981}:真正的要点"), "真正的要点");
        assert_eq!(sanitize_summary("Summary: the real point"), "the real point");
    }

    #[test]
    fn sanitize_caps_length() {
        let long = "字".repeat(300);
        let out = sanitize_summary(&long);
        assert_eq!(out.chars().count(), MAX_SUMMARY_CHARS);
    }

    #[test]
    fn task_guards() {
        let mut entry = UserTaskResultEntry {
            id: "r".into(),
            cron_job_id: "j".into(),
            user_id: "u".into(),
            output: "x".repeat(300),
            status: "success".into(),
            executed_at: "t".into(),
            job_name: None,
            job_prompt: None,
            summary: String::new(),
        };
        assert!(should_summarize_task(&entry));
        entry.status = "error".into();
        assert!(!should_summarize_task(&entry));
        entry.status = "success".into();
        entry.output = "short".into();
        assert!(!should_summarize_task(&entry));
    }

    #[test]
    fn doc_guards() {
        assert!(should_summarize_doc(&"内容".repeat(100)));
        assert!(!should_summarize_doc("太短"));
        assert!(!should_summarize_doc(&format!(
            "[Document parse pending: docling not ready] {}",
            "x".repeat(300)
        )));
        assert!(!should_summarize_doc(&format!(
            "[Excel import error: bad file] {}",
            "x".repeat(300)
        )));
    }

    #[test]
    fn task_message_includes_context_and_truncates() {
        let msg = build_task_message(Some("每日新闻"), Some("总结今天的AI新闻"), &"行".repeat(5000));
        assert!(msg.contains("任务名:每日新闻"));
        assert!(msg.contains("任务指令:总结今天的AI新闻"));
        assert!(msg.contains("执行结果(可能截断)"));
        assert!(msg.chars().count() < 4300, "output must be truncated");
        let msg_no_meta = build_task_message(None, None, "正文");
        assert!(msg_no_meta.contains("正文"));
        assert!(!msg_no_meta.contains("任务名"));
    }

    #[test]
    fn doc_message_uses_filename() {
        let msg = build_doc_message("docs/report.md", "正文内容");
        assert!(msg.contains("文件名:report.md"));
        assert!(msg.contains("正文内容"));
    }
}
