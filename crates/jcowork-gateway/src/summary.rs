//! AI summaries for the aggregated feed.
//!
//! Task results and indexed documents each get a short LLM-written teaser
//! stored next to them, so feed cards show a punchy hook instead of a raw
//! content truncation. Generation is fire-and-forget: the write paths and
//! the feed reader call the `ensure_*` methods, an in-flight set dedups
//! concurrent attempts, and any failure simply leaves the raw truncation
//! in place — the next feed view retries.
//!
//! Opening a document in the feed additionally requests a long-form
//! core-content summary ([`SummaryService::summarize_document_detail`]),
//! generated synchronously from the indexed text and stored per document.
//! It is invalidated whenever the indexed content changes.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use tokio::sync::Semaphore;

use jcowork_cron::{CronScheduler, UserTaskResultEntry};
use jcowork_llm::LlmRouter;
use jcowork_llm::provider::ChatMessage;
use jcowork_storage::{DetailSummarySource, UserSettingsStore, WorkspaceIndex};

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
/// Hard cap on the long-form summary shown above a document (Markdown).
const DETAIL_MAX_CHARS: usize = 1200;

const SYSTEM_PROMPT: &str = "你是「AI头条」信息流的编辑,任务是给一段内容写一句让人想点进去看完整的摘要。规则:
- 用内容本身的语言写(中文内容写中文,英文内容写英文,以此类推)
- 一到两句话,不超过 60 个汉字(英文内容不超过 120 字符)
- 第一句就给出内容里最有价值、最具体的信息点或结论,不写空洞铺垫
- 可以制造合理的好奇心,但严禁编造内容中不存在的信息,不用无实质的夸张词
- 只输出摘要正文:不要标题、不要引号、不要 Markdown、不要「摘要:」之类前缀";

const DETAIL_SYSTEM_PROMPT: &str = "你是「AI头条」的文档编辑,任务是为用户正在阅读的文档写一份核心内容摘要,让他快速判断文档讲了什么、值不值得通读。规则:
- 用文档本身的语言写(中文文档写中文,英文文档写英文,以此类推)
- 第一段用一到三句话概括文档的主旨和最重要的结论
- 随后另起一段,用「- 」列出 3-5 条关键要点,每条突出具体的事实、数字、结论或行动项,不超过 40 个汉字(英文不超过 80 字符)
- 直接给内容,不写「本文」「该文档」之类的空泛铺垫,不写开场白
- 严禁编造文档中不存在的信息
- 只输出摘要正文:不要标题、不要代码块、不要「摘要:」之类前缀";

/// Why a long-form document summary could not be produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailSummaryError {
    /// Not indexed (or not public, for feed visitors).
    NotFound,
    /// The index holds no usable text (parse failed or still pending).
    NoContent,
    /// The model call failed or returned nothing usable.
    Failed,
}

pub struct SummaryService {
    llm_router: Arc<RwLock<LlmRouter>>,
    /// Per-user model selection (the model picked in Settings). Summaries run
    /// on the same model as the user's conversations, so a deployment default
    /// whose key is stale does not silently disable every summary.
    user_settings: Arc<UserSettingsStore>,
    default_model: String,
    /// Keys of summaries currently generating ("task:{id}" / "doc:{user}:{path}").
    inflight: Mutex<HashSet<String>>,
    /// Per-document locks serializing long-form summary generation: the
    /// first request generates, later ones wait and read the stored copy.
    detail_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    sem: Arc<Semaphore>,
}

impl SummaryService {
    pub fn new(
        llm_router: Arc<RwLock<LlmRouter>>,
        user_settings: Arc<UserSettingsStore>,
        default_model: String,
    ) -> Self {
        Self {
            llm_router,
            user_settings,
            default_model,
            inflight: Mutex::new(HashSet::new()),
            detail_locks: Mutex::new(HashMap::new()),
            sem: Arc::new(Semaphore::new(MAX_CONCURRENT)),
        }
    }

    /// Fire-and-forget: generate and persist a teaser for a task result that
    /// does not have one yet. No-op when a generation is already running.
    /// `model_hint` is the model the run itself used, when known.
    pub fn ensure_task_summary(
        self: &Arc<Self>,
        cron_scheduler: Arc<CronScheduler>,
        result_id: &str,
        model_hint: Option<&str>,
    ) {
        let key = format!("task:{}", result_id);
        if !self.try_begin(&key) {
            return;
        }
        let svc = Arc::clone(self);
        let result_id = result_id.to_string();
        let model_hint = model_hint.filter(|m| !m.trim().is_empty()).map(str::to_string);
        tokio::spawn(async move {
            let _permit = svc.acquire_slot().await;
            generate_task_summary(&svc, &cron_scheduler, &result_id, model_hint.as_deref()).await;
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

    /// Long-form AI summary of one indexed document, shown above the content
    /// when a feed card is opened. The first request pays the LLM call and
    /// stores the result; later requests read the stored copy.
    pub async fn summarize_document_detail(
        &self,
        data_dir: &str,
        user_id: &str,
        file_path: &str,
        public_only: bool,
    ) -> Result<String, DetailSummaryError> {
        let index = WorkspaceIndex::cached(data_dir, user_id)
            .await
            .map_err(|e| {
                tracing::warn!(error = %e, user_id = %user_id, "Detail summary: index open failed");
                DetailSummaryError::Failed
            })?;
        let mut source = index
            .get_detail_summary_source(file_path, public_only)
            .await
            .map_err(|e| {
                tracing::warn!(error = %e, file = %file_path, "Detail summary: source read failed");
                DetailSummaryError::Failed
            })?
            .ok_or(DetailSummaryError::NotFound)?;
        if !source.detail_summary.is_empty() {
            return Ok(source.detail_summary);
        }
        // HTML is stored as raw markup for the preview iframe; feed the model
        // the extracted text instead, or the prompt is mostly CSS and tags.
        if is_html_file(file_path) {
            match index.get_plain_text(file_path).await {
                Ok(Some(text)) => source = detail_source_from_text(&source, &text),
                Ok(None) => return Err(DetailSummaryError::NotFound),
                Err(e) => {
                    tracing::warn!(error = %e, file = %file_path, "Detail summary: text extraction failed")
                }
            }
        }
        let head = source.head.trim();
        if head.is_empty() || is_parse_placeholder(head) {
            return Err(DetailSummaryError::NoContent);
        }

        // Serialize requests for the same document: one generates, the rest
        // block here and then read the stored result.
        let key = format!("{}:{}", user_id, file_path);
        let lock = {
            let mut locks = self.detail_locks.lock().expect("detail summary lock poisoned");
            Arc::clone(locks.entry(key).or_default())
        };
        let _guard = lock.lock().await;

        // Another request may have finished while this one waited.
        if let Ok(Some(src)) = index.get_detail_summary_source(file_path, public_only).await {
            if !src.detail_summary.is_empty() {
                return Ok(src.detail_summary);
            }
        }

        let _permit = self.acquire_slot().await;
        let model = self.resolve_model(user_id, None).await;
        let user_message = build_doc_detail_message(&source);
        let raw = self
            .generate_with_prompt(DETAIL_SYSTEM_PROMPT, &user_message, &model)
            .await
            .ok_or(DetailSummaryError::Failed)?;
        let summary = sanitize_detail_summary(&raw);
        if summary.is_empty() {
            return Err(DetailSummaryError::Failed);
        }
        if let Err(e) = index.set_document_detail_summary(file_path, &summary).await {
            tracing::warn!(error = %e, file = %file_path, "Detail summary: store failed");
        }
        tracing::info!(
            user_id = %user_id,
            file = %file_path,
            len = summary.chars().count(),
            "Document detail summary generated"
        );
        Ok(summary)
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

    /// Model for one user's summaries: the hint (the model a cron run actually
    /// used), else their saved selection, else the deployment default.
    async fn resolve_model(&self, user_id: &str, hint: Option<&str>) -> String {
        if let Some(model) = hint.map(str::trim).filter(|m| !m.is_empty()) {
            return model.to_string();
        }
        match self.user_settings.get_model(user_id).await {
            Ok(Some(model)) if !model.trim().is_empty() => model,
            Ok(_) => self.default_model.clone(),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    user_id = %user_id,
                    "Summary generation: saved model unreadable; using the default"
                );
                self.default_model.clone()
            }
        }
    }

    /// One non-streaming LLM call through the resolved model, used for the
    /// card teaser (then trimmed to a single line).
    /// Returns None on any failure (already logged); the caller keeps the
    /// raw truncation as the card text.
    async fn generate(&self, user_message: &str, model: &str) -> Option<String> {
        let raw = self.generate_with_prompt(SYSTEM_PROMPT, user_message, model).await?;
        let summary = sanitize_summary(&raw);
        if summary.is_empty() {
            tracing::warn!(model = %model, "Summary generation: model returned no usable text");
            None
        } else {
            Some(summary)
        }
    }

    /// One non-streaming LLM call with the given system prompt. A configured
    /// model whose provider has since disappeared falls back to the default.
    /// Returns None on any failure (already logged).
    async fn generate_with_prompt(
        &self,
        system_prompt: &str,
        user_message: &str,
        model: &str,
    ) -> Option<String> {
        let (model, provider) = {
            let router = self.llm_router.read().ok()?;
            match router.get_provider(model) {
                Ok(p) => (model.to_string(), p),
                Err(e) if model != self.default_model => {
                    tracing::warn!(
                        error = %e,
                        model = %model,
                        fallback = %self.default_model,
                        "Summary generation: model unavailable; using the default model"
                    );
                    match router.get_provider(&self.default_model) {
                        Ok(p) => (self.default_model.clone(), p),
                        Err(e) => {
                            tracing::warn!(
                                error = %e,
                                model = %self.default_model,
                                "Summary generation: provider unavailable"
                            );
                            return None;
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        model = %model,
                        "Summary generation: provider unavailable"
                    );
                    return None;
                }
            }
        };
        let messages = [
            ChatMessage {
                role: "system".to_string(),
                content: system_prompt.to_string(),
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
                if resp.message.content.trim().is_empty() {
                    tracing::warn!(model = %model, "Summary generation: model returned no usable text");
                    None
                } else {
                    Some(resp.message.content)
                }
            }
            Ok(Err(e)) => {
                tracing::warn!(error = %e, model = %model, "Summary generation: LLM call failed");
                None
            }
            Err(_) => {
                tracing::warn!(
                    secs = LLM_TIMEOUT_SECS,
                    model = %model,
                    "Summary generation: LLM call timed out"
                );
                None
            }
        }
    }
}

async fn generate_task_summary(
    svc: &SummaryService,
    cron_scheduler: &CronScheduler,
    result_id: &str,
    model_hint: Option<&str>,
) {
    let entry = match cron_scheduler.get_task_result_by_id(result_id).await {
        Some(e) => e,
        None => return, // deleted before the job ran — nothing to do
    };
    if !should_summarize_task(&entry) {
        return; // already summarized, failed, or short enough to show as-is
    }
    let model = svc.resolve_model(&entry.user_id, model_hint).await;
    let user_message = build_task_message(
        entry.job_name.as_deref(),
        entry.job_prompt.as_deref(),
        &entry.output,
    );
    if let Some(summary) = svc.generate(&user_message, &model).await {
        cron_scheduler.set_task_result_summary(result_id, &summary).await;
        tracing::info!(result_id = %result_id, model = %model, len = summary.chars().count(), "Task result summary stored");
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
    // HTML is indexed as raw markup, so its head is mostly CSS and tags;
    // summarize the extracted text instead.
    let head = if is_html_file(file_path) {
        match index.get_plain_text(file_path).await {
            Ok(Some(text)) => text.chars().take(SOURCE_HEAD_CHARS).collect(),
            Ok(None) => return, // document no longer indexed
            Err(e) => {
                tracing::warn!(error = %e, file = %file_path, "Summary generation: text extraction failed");
                source.head.clone()
            }
        }
    } else {
        source.head.clone()
    };
    if !should_summarize_doc(&head) {
        return; // placeholder text, or short enough to show as-is
    }
    let model = svc.resolve_model(user_id, None).await;
    let user_message = build_doc_message(file_path, &head);
    if let Some(summary) = svc.generate(&user_message, &model).await {
        if let Err(e) = index.set_document_summary(file_path, &summary).await {
            tracing::warn!(error = %e, file = %file_path, "Summary generation: store failed");
        } else {
            tracing::info!(file = %file_path, model = %model, len = summary.chars().count(), "Document summary stored");
        }
    }
}

/// True for files whose indexed text is raw HTML markup.
fn is_html_file(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".html") || lower.ends_with(".htm")
}

/// Rebuild a long-form summary source from extracted plain text, preserving
/// the head/tail sampling contract used by [`build_doc_detail_message`].
fn detail_source_from_text(source: &DetailSummarySource, text: &str) -> DetailSummarySource {
    let total_len = text.chars().count() as i64;
    let head_len = total_len.min(6000);
    let head: String = text.chars().take(head_len as usize).collect();
    let tail: String = if total_len > 6000 {
        text.chars().skip((total_len - 3000) as usize).collect()
    } else {
        String::new()
    };
    DetailSummarySource {
        detail_summary: source.detail_summary.clone(),
        filename: source.filename.clone(),
        total_len,
        head_len,
        head,
        tail,
    }
}

/// A task result earns a summary only when it succeeded and its output is
/// long enough that the raw excerpt does not already tell the whole story.
fn should_summarize_task(entry: &UserTaskResultEntry) -> bool {
    entry.status == "success" && entry.output.trim().chars().count() >= MIN_SOURCE_CHARS
}

/// True when indexed text is a parse-error placeholder rather than content.
fn is_parse_placeholder(text: &str) -> bool {
    let t = text.trim_start();
    t.starts_with("[Document parse") || t.starts_with("[Excel import error")
}

/// A document earns a teaser summary only when its extracted text is real
/// content and long enough to hide anything.
fn should_summarize_doc(head: &str) -> bool {
    let t = head.trim();
    t.chars().count() >= MIN_SOURCE_CHARS && !is_parse_placeholder(t)
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

/// Prompt body for a long-form summary. Long documents contribute their
/// opening and closing so both the thesis and the conclusions are visible;
/// the omitted middle is marked so the model does not invent continuity.
pub(crate) fn build_doc_detail_message(source: &DetailSummarySource) -> String {
    let mut m = format!(
        "【文档】\n文件名:{}\n内容(可能截断):\n{}",
        source.filename, source.head
    );
    // The head covers the first 6000 characters. Append only the trailing
    // characters the head does not already contain, so the two samples
    // neither overlap nor leave a gap; mark the omitted middle explicitly.
    let missing = (source.total_len - source.head_len).max(0);
    if missing > 0 {
        let tail_len = source.tail.chars().count() as i64;
        let take = missing.min(tail_len);
        if take > 0 {
            if missing > take {
                m.push_str("\n[...中间部分省略...]\n");
            }
            let tail: String = source
                .tail
                .chars()
                .skip((tail_len - take) as usize)
                .collect();
            m.push_str(&tail);
        }
    }
    m
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

/// Clean up a model answer into a multi-paragraph Markdown summary.
/// Structure (blank-line paragraphs, `- ` bullets) is preserved; only a
/// wrapping fence, a stray lead-in label, and excess blank lines go away.
pub(crate) fn sanitize_detail_summary(raw: &str) -> String {
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

    // Strip a "Summary:"-style label the model was told not to emit.
    for p in ["\u{6458}\u{8981}\u{ff1a}", "\u{6458}\u{8981}:", "Summary:", "summary:"] {
        if let Some(rest) = s.strip_prefix(p) {
            s = rest.trim_start();
            break;
        }
    }

    // Collapse runs of blank lines so the rendered paragraphs stay tight.
    let mut out = String::with_capacity(s.len());
    let mut blank = 0;
    for line in s.lines() {
        if line.trim().is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }

    let out = out.trim().to_string();
    if out.chars().count() > DETAIL_MAX_CHARS {
        out.chars().take(DETAIL_MAX_CHARS).collect()
    } else {
        out
    }
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

    fn detail_source(total_len: i64, head: &str, tail: &str) -> DetailSummarySource {
        DetailSummarySource {
            detail_summary: String::new(),
            filename: "report.md".into(),
            total_len,
            head_len: head.chars().count() as i64,
            head: head.into(),
            tail: tail.into(),
        }
    }

    #[test]
    fn detail_message_uses_filename() {
        let msg = build_doc_detail_message(&detail_source(4, "正文内容", "正文内容"));
        assert!(msg.contains("文件名:report.md"));
        assert!(msg.contains("正文内容"));
        assert!(!msg.contains("中间部分省略"), "complete content needs no marker");
    }

    #[test]
    fn detail_message_samples_head_and_tail_of_long_documents() {
        let msg = build_doc_detail_message(&detail_source(20_000, "开头", "结尾"));
        assert!(msg.contains("开头"));
        assert!(msg.contains("结尾"));
        assert!(msg.contains("中间部分省略"));
    }

    #[test]
    fn detail_message_joins_partial_tail_without_duplication() {
        // Only one character is missing, so just the last tail character is
        // appended and no omission marker is needed.
        let msg = build_doc_detail_message(&detail_source(5, "正文内容", "四五六"));
        assert!(msg.contains("正文内容六"));
        assert!(!msg.contains("中间部分省略"));
    }

    #[test]
    fn detail_sanitize_keeps_markdown_structure() {
        let raw = "\u{6458}\u{8981}\u{ff1a}这份报告讲了三件事。\n\n\n\n- 收入增长 20%\n- 成本下降 5%\n\n";
        assert_eq!(
            sanitize_detail_summary(raw),
            "这份报告讲了三件事。\n\n- 收入增长 20%\n- 成本下降 5%"
        );
        assert_eq!(
            sanitize_detail_summary("```markdown\n第一段\n\n- 要点\n```"),
            "第一段\n\n- 要点"
        );
    }

    #[test]
    fn detail_sanitize_caps_length() {
        let long = "字".repeat(2000);
        assert_eq!(sanitize_detail_summary(&long).chars().count(), DETAIL_MAX_CHARS);
    }

    #[test]
    fn detail_jobs_still_reject_placeholders() {
        assert!(is_parse_placeholder("[Document parse pending: docling not ready] text"));
        assert!(!is_parse_placeholder("# Real title"));
    }

    #[test]
    fn html_files_are_detected_by_extension() {
        assert!(is_html_file("docs/report.html"));
        assert!(is_html_file("docs/REPORT.HTM"));
        assert!(!is_html_file("docs/report.md"));
    }

    #[test]
    fn detail_source_from_text_samples_head_and_tail() {
        let text: String = "字".repeat(10_000);
        let src = detail_source_from_text(&detail_source(0, "", ""), &text);
        assert_eq!(src.total_len, 10_000);
        assert_eq!(src.head.chars().count(), 6000);
        assert_eq!(src.tail.chars().count(), 3000);
        let msg = build_doc_detail_message(&src);
        assert!(msg.contains("中间部分省略"));
    }

    #[tokio::test]
    async fn resolve_model_prefers_the_users_saved_choice() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("open in-memory settings db");
        jcowork_storage::migration::run_migrations(&pool)
            .await
            .expect("run migrations");
        let settings = Arc::new(UserSettingsStore::new(pool));
        settings
            .set_model("u1", Some("deepseek:deepseek-v4-flash"))
            .await
            .unwrap();

        let svc = SummaryService::new(
            Arc::new(RwLock::new(LlmRouter::new())),
            settings,
            "moonshot:kimi-k2.6".to_string(),
        );

        assert_eq!(
            svc.resolve_model("u1", None).await,
            "deepseek:deepseek-v4-flash"
        );
        // Users without a saved choice keep the deployment default.
        assert_eq!(svc.resolve_model("u2", None).await, "moonshot:kimi-k2.6");
        // A run hint wins: the teaser uses the model the run itself used.
        assert_eq!(
            svc.resolve_model("u1", Some("zhipu:glm-5")).await,
            "zhipu:glm-5"
        );
    }
}
