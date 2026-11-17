//! Structured outcome of a tool call.
//!
//! Tool results reach the agent loop as `Result<String>`, so a failure is not
//! always an `Err`: several tools answer `Ok("Failed to add cron job: ...")`
//! or `Ok("web_search: timed out after 60s")`. Usage analysis that only looks
//! for an `Error:` prefix therefore undercounts failures. Every result is
//! classified here — at the single point where the loop logs it — into a
//! machine-readable verdict that is persisted with the log entry.
//!
//! New tools should return `Err` on failure; the text rules below exist for
//! the payloads tools already produce.

/// Error kinds written to tool-call logs.
pub mod kinds {
    /// The call exceeded the loop's per-tool timeout.
    pub const TIMEOUT: &str = "timeout";
    /// The requested tool is not registered.
    pub const UNKNOWN_TOOL: &str = "unknown_tool";
    /// The tool task panicked.
    pub const CRASH: &str = "crash";
    /// Any other `Err` returned by the tool.
    pub const ERROR: &str = "error";
    /// A subprocess exited with a non-zero status.
    pub const SUBPROCESS: &str = "subprocess";
    /// The tool returned `Ok` with text describing a failure.
    pub const REPORTED: &str = "reported";
}

/// Verdict of one tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolOutcome {
    pub ok: bool,
    /// One of [`kinds`] when `ok` is false.
    pub error_kind: Option<&'static str>,
}

impl ToolOutcome {
    /// The call succeeded.
    pub const SUCCESS: Self = Self {
        ok: true,
        error_kind: None,
    };

    /// The call failed with the given kind.
    pub const fn failure(kind: &'static str) -> Self {
        Self {
            ok: false,
            error_kind: Some(kind),
        }
    }
}

/// Classify the `Err` branch of a dispatch.
pub fn classify_error(message: &str) -> ToolOutcome {
    let lower = message.to_ascii_lowercase();
    let kind = if lower.contains("timed out") || lower.contains("timeout") {
        kinds::TIMEOUT
    } else if lower.contains("unknown tool") {
        kinds::UNKNOWN_TOOL
    } else if lower.contains("crashed") {
        kinds::CRASH
    } else {
        kinds::ERROR
    };
    ToolOutcome::failure(kind)
}

/// Classify the `Ok` branch of a dispatch by its payload text.
///
/// `tool` is the requested tool name: tools that self-report a failure prefix
/// the payload with their own name (`web_search: timed out after 60s`), which
/// keeps ordinary result content from being mistaken for an error.
pub fn classify_output(tool: &str, output: &str) -> ToolOutcome {
    let first_line = output.trim_start().lines().next().unwrap_or("");
    let lower = first_line.to_ascii_lowercase();

    // "[exit code 7] ..." — the shell tool reports a failing subprocess this way.
    if let Some(rest) = lower.strip_prefix("[exit code ") {
        let code = rest.split(']').next().unwrap_or("").trim();
        if code != "0" {
            return ToolOutcome::failure(kinds::SUBPROCESS);
        }
    }

    // Generic failure prefixes returned as a successful result.
    if lower.starts_with("error:")
        || lower.starts_with("error ")
        || lower.starts_with("failed to ")
        || lower.starts_with("failed:")
    {
        return ToolOutcome::failure(kinds::REPORTED);
    }

    // "<tool>: timed out after 60s" / "<tool> error: ..." / "<tool>: failed ..."
    let name = tool.trim().to_ascii_lowercase();
    if !name.is_empty() && lower.starts_with(&name) {
        let rest = lower[name.len()..]
            .trim_start_matches(|c: char| c == ':' || c.is_whitespace());
        if rest.starts_with("timed out") || rest.starts_with("timeout") {
            return ToolOutcome::failure(kinds::TIMEOUT);
        }
        if rest.starts_with("error") || rest.starts_with("failed") || rest.starts_with("unexpected output")
        {
            return ToolOutcome::failure(kinds::REPORTED);
        }
    }

    ToolOutcome::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn self_reported_failures_are_detected() {
        // Real payloads from the search, cron and memory tools.
        assert_eq!(
            classify_output("web_search", "web_search: timed out after 60s"),
            ToolOutcome::failure(kinds::TIMEOUT)
        );
        assert_eq!(
            classify_output(
                "web_search",
                "web_search: failed to spawn process: No such file"
            ),
            ToolOutcome::failure(kinds::REPORTED)
        );
        assert_eq!(
            classify_output("web_search", "web_search error: blocked"),
            ToolOutcome::failure(kinds::REPORTED)
        );
        assert_eq!(
            classify_output("cron_add", "Failed to add cron job: invalid schedule"),
            ToolOutcome::failure(kinds::REPORTED)
        );
        assert_eq!(
            classify_output("memory_save", "Failed to save memory: disk full"),
            ToolOutcome::failure(kinds::REPORTED)
        );
        assert_eq!(
            classify_output("file_read", "Error: missing file"),
            ToolOutcome::failure(kinds::REPORTED)
        );
    }

    #[test]
    fn shell_non_zero_exit_counts_as_subprocess_failure() {
        assert_eq!(
            classify_output("shell", "[exit code 1] \n[stderr] ls: not found"),
            ToolOutcome::failure(kinds::SUBPROCESS)
        );
        assert_eq!(
            classify_output("shell", "[exit code 0] done"),
            ToolOutcome::SUCCESS
        );
    }

    #[test]
    fn ordinary_results_are_not_mistaken_for_failures() {
        assert_eq!(
            classify_output(
                "web_search",
                "Web search results for \"rust\":\n\n1. Rust"
            ),
            ToolOutcome::SUCCESS
        );
        assert_eq!(
            classify_output("web_search", "No search results found."),
            ToolOutcome::SUCCESS
        );
        assert_eq!(
            classify_output("file_read", "(empty or not found)"),
            ToolOutcome::SUCCESS
        );
        // Content mentioning errors is fine as long as the prefix is not a verdict.
        assert_eq!(
            classify_output("file_read", "fn log_error() { /* error: ... */ }"),
            ToolOutcome::SUCCESS
        );
        assert_eq!(classify_output("todo_add", "Todo added (stub)"), ToolOutcome::SUCCESS);
    }

    #[test]
    fn dispatch_errors_are_classified_by_cause() {
        assert_eq!(
            classify_error("Tool call timed out"),
            ToolOutcome::failure(kinds::TIMEOUT)
        );
        assert_eq!(
            classify_error("Unknown tool: nope"),
            ToolOutcome::failure(kinds::UNKNOWN_TOOL)
        );
        assert_eq!(
            classify_error("Tool 'shell' crashed: task panicked"),
            ToolOutcome::failure(kinds::CRASH)
        );
        assert_eq!(
            classify_error("permission denied"),
            ToolOutcome::failure(kinds::ERROR)
        );
    }
}
