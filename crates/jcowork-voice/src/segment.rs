//! Sentence segmentation for streaming TTS.
//!
//! Agent replies arrive as markdown text deltas. The TTS stream wants plain
//! spoken sentences: this segmenter strips markdown decoration, skips code
//! blocks entirely (reading code aloud is useless), and emits a sentence once
//! a terminal punctuation mark is seen and the buffer is long enough to be
//! worth synthesizing on its own.

/// Minimum characters before a sentence is emitted on punctuation. Short
/// fragments accumulate and are released by the next boundary or `flush`.
const MIN_SENTENCE_CHARS: usize = 8;

/// Incremental markdown-to-speech sentence splitter.
pub struct SpeechSegmenter {
    /// Cleaned text of the sentence being accumulated.
    sentence: String,
    /// Consecutive backticks seen (``` toggles code-block mode).
    backticks: usize,
    in_code: bool,
    /// Text collected inside `[...]` (link label or image alt).
    bracket: Option<String>,
    /// Whether the bracket was preceded by `!` (image — drop the label).
    bracket_is_image: bool,
    /// Skipping a `(...)` URL after a link label.
    in_url: bool,
    prev_char: Option<char>,
    at_line_start: bool,
}

impl SpeechSegmenter {
    pub fn new() -> Self {
        Self {
            sentence: String::new(),
            backticks: 0,
            in_code: false,
            bracket: None,
            bracket_is_image: false,
            in_url: false,
            prev_char: None,
            at_line_start: true,
        }
    }

    /// Feed a text delta; returns any sentences completed by it.
    pub fn push(&mut self, delta: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut chars = delta.chars().peekable();
        while let Some(ch) = chars.next() {
            // Code fences: three consecutive backticks toggle code mode.
            if ch == '`' {
                self.backticks += 1;
                if self.backticks == 3 {
                    self.in_code = !self.in_code;
                    self.backticks = 0;
                }
                self.prev_char = Some(ch);
                continue;
            }
            self.backticks = 0;
            if self.in_code {
                if ch == '\n' {
                    self.at_line_start = true;
                }
                self.prev_char = Some(ch);
                continue;
            }

            // Link/image URL: skip until the closing paren, then release
            // the label (links) or drop it (images).
            if self.in_url {
                if ch == ')' {
                    self.in_url = false;
                    if !self.bracket_is_image {
                        if let Some(label) = self.bracket.take() {
                            self.sentence.push_str(&label);
                        }
                    } else {
                        self.bracket = None;
                    }
                }
                self.prev_char = Some(ch);
                continue;
            }

            // Inside a [label]: collect until ']'.
            if let Some(label) = self.bracket.as_mut() {
                if ch == ']' {
                    let label = self.bracket.take().unwrap();
                    if chars.peek() == Some(&'(') {
                        chars.next();
                        self.in_url = true;
                        self.bracket = Some(label);
                    } else {
                        // Not a link — keep the literal text.
                        self.sentence.push('[');
                        self.sentence.push_str(&label);
                        self.sentence.push(']');
                    }
                } else {
                    label.push(ch);
                }
                self.prev_char = Some(ch);
                continue;
            }

            match ch {
                '[' => {
                    self.bracket = Some(String::new());
                    self.bracket_is_image = self.prev_char == Some('!');
                    if self.bracket_is_image {
                        // Remove the '!' already emitted for the image marker.
                        if self.sentence.ends_with('!') {
                            self.sentence.pop();
                        }
                    }
                }
                // Markdown decoration that should not be spoken.
                '*' | '#' | '>' | '|' => {
                    if ch == '|' {
                        self.sentence.push(' ');
                    }
                }
                '\r' => {}
                '\n' => {
                    self.at_line_start = true;
                    self.emit_if_ready(&mut out, true);
                }
                '。' | '！' | '？' | '!' | '?' | '；' | ';' => {
                    // A '!' right before '[' is an image marker, not punctuation.
                    if ch == '!' && chars.peek() == Some(&'[') {
                        self.sentence.push(ch);
                    } else {
                        self.sentence.push(ch);
                        self.emit_if_ready(&mut out, false);
                    }
                }
                _ => {
                    // Skip list bullets at line start.
                    if self.at_line_start && (ch == '-' || ch == '+' || ch == '•') {
                        self.prev_char = Some(ch);
                        continue;
                    }
                    if ch != ' ' && ch != '\t' {
                        self.at_line_start = false;
                    }
                    if ch == ' ' || ch == '\t' {
                        // Collapse whitespace runs.
                        if !self.sentence.ends_with(' ') && !self.sentence.is_empty() {
                            self.sentence.push(' ');
                        }
                    } else {
                        self.sentence.push(ch);
                    }
                }
            }
            self.prev_char = Some(ch);
        }
        out
    }

    /// Release any remaining text (call when the reply is done).
    pub fn flush(&mut self) -> Option<String> {
        let text = self.sentence.trim().to_string();
        self.sentence.clear();
        if text.is_empty() { None } else { Some(text) }
    }

    fn emit_if_ready(&mut self, out: &mut Vec<String>, newline: bool) {
        let trimmed = self.sentence.trim();
        let len = trimmed.chars().count();
        let ready = if newline {
            len >= MIN_SENTENCE_CHARS
        } else {
            len >= MIN_SENTENCE_CHARS || (len > 0 && ends_strong(trimmed))
        };
        if ready {
            out.push(trimmed.to_string());
            self.sentence.clear();
        } else if newline {
            // Keep accumulating, but normalize the line break to a space.
            if !self.sentence.is_empty() && !self.sentence.ends_with(' ') {
                self.sentence.push(' ');
            }
        }
    }
}

impl Default for SpeechSegmenter {
    fn default() -> Self {
        Self::new()
    }
}

/// Strong terminal punctuation emits even short sentences ("好的。").
fn ends_strong(s: &str) -> bool {
    s.ends_with('。') || s.ends_with('！') || s.ends_with('？')
}

#[cfg(test)]
mod tests {
    use super::SpeechSegmenter;

    #[test]
    fn splits_on_chinese_punctuation() {
        let mut seg = SpeechSegmenter::new();
        let out = seg.push("今天天气非常不错。我们下午一起去公园散步吧。");
        assert_eq!(
            out,
            vec!["今天天气非常不错。", "我们下午一起去公园散步吧。"]
        );
        assert_eq!(seg.flush(), None);
    }

    #[test]
    fn short_fragments_accumulate_until_flush() {
        let mut seg = SpeechSegmenter::new();
        // "好的。" is strong punctuation — emitted immediately.
        let out = seg.push("好的。");
        assert_eq!(out, vec!["好的。"]);
        // A weak boundary under the minimum stays buffered.
        let out = seg.push("稍等;");
        assert!(out.is_empty());
        assert_eq!(seg.flush().as_deref(), Some("稍等;"));
    }

    #[test]
    fn deltas_split_across_pushes() {
        let mut seg = SpeechSegmenter::new();
        assert!(seg.push("今天天气").is_empty());
        assert!(seg.push("非常不错").is_empty());
        let out = seg.push("。接下来");
        assert_eq!(out, vec!["今天天气非常不错。"]);
        assert_eq!(seg.flush().as_deref(), Some("接下来"));
    }

    #[test]
    fn code_blocks_are_skipped() {
        let mut seg = SpeechSegmenter::new();
        let out = seg.push("下面是示例代码：\n```python\nprint('hello world')\n```\n这段代码可以直接运行。");
        assert!(out.iter().all(|s| !s.contains("print")));
        let mut all = out.join("");
        if let Some(rest) = seg.flush() {
            all.push_str(&rest);
        }
        assert!(all.contains("这段代码可以直接运行。"));
        assert!(!all.contains("hello world"));
    }

    #[test]
    fn markdown_decoration_stripped() {
        let mut seg = SpeechSegmenter::new();
        let out = seg.push("**重点内容**如下所示。");
        assert_eq!(out, vec!["重点内容如下所示。"]);

        let mut seg = SpeechSegmenter::new();
        let out = seg.push("参见[官方文档](https://example.com/docs)获取更多信息。");
        assert_eq!(out, vec!["参见官方文档获取更多信息。"]);

        let mut seg = SpeechSegmenter::new();
        let out = seg.push("![示意图](https://example.com/a.png)如图所示。");
        assert_eq!(out, vec!["如图所示。"]);
    }

    #[test]
    fn headings_and_lists() {
        let mut seg = SpeechSegmenter::new();
        let out = seg.push("## 标题说明\n- 第一个要点内容如下\n");
        let joined = out.join("");
        assert!(!joined.contains('#'));
        assert!(!joined.contains("- "));
        assert!(joined.contains("标题说明"));
        assert!(joined.contains("第一个要点内容如下"));
    }
}