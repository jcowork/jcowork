//! Readable plain-text extraction from HTML.
//!
//! The workspace index deliberately stores the original HTML markup:
//! the feed's document detail renders it in an iframe. Feed snippets and
//! summary prompts therefore extract text at read time instead of mutating
//! the indexed content.

/// Extract readable plain text from an HTML document.
///
/// Drops comments, `<script>`/`<style>` bodies and tags, decodes the common
/// entities, and collapses whitespace runs into single spaces.
pub fn html_to_plain_text(html: &str) -> String {
    // ASCII case folding keeps byte offsets aligned with `html`, so all the
    // slicing below stays on char boundaries (markers are ASCII).
    let lower = html.to_ascii_lowercase();
    let mut text = String::with_capacity(html.len() / 2);
    let mut i = 0usize;

    while i < html.len() {
        if lower[i..].starts_with("<!--") {
            i = match lower[i + 4..].find("-->") {
                Some(end) => i + 4 + end + 3,
                None => html.len(),
            };
            text.push(' ');
        } else if lower[i..].starts_with("<script") || lower[i..].starts_with("<style") {
            let close = if lower[i..].starts_with("<script") {
                "</script"
            } else {
                "</style"
            };
            i = match lower[i..].find(close) {
                Some(end) => {
                    let after = i + end + close.len();
                    lower[after..]
                        .find('>')
                        .map(|gt| after + gt + 1)
                        .unwrap_or(html.len())
                }
                None => html.len(),
            };
            text.push(' ');
        } else if lower[i..].starts_with('<') {
            i = lower[i..]
                .find('>')
                .map(|end| i + end + 1)
                .unwrap_or(html.len());
            text.push(' ');
        } else if lower[i..].starts_with('&') {
            let entity_end = lower[i..]
                .find(';')
                .map(|end| i + end + 1)
                .filter(|&end| end - i <= 12);
            match entity_end.and_then(|end| decode_entity(&html[i..end]).map(|c| (c, end))) {
                Some((c, end)) => {
                    text.push(c);
                    i = end;
                }
                None => {
                    text.push('&');
                    i += 1;
                }
            }
        } else {
            let next = lower[i..]
                .find(['<', '&'])
                .map(|offset| i + offset)
                .unwrap_or(html.len());
            text.push_str(&html[i..next]);
            i = next;
        }
    }

    let mut out = String::with_capacity(text.len());
    for word in text.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}

/// Decode one `&...;` entity. Returns `None` for unknown names.
fn decode_entity(entity: &str) -> Option<char> {
    let known = match entity.to_ascii_lowercase().as_str() {
        "&amp;" => Some('&'),
        "&lt;" => Some('<'),
        "&gt;" => Some('>'),
        "&quot;" => Some('"'),
        "&apos;" => Some('\''),
        "&nbsp;" => Some(' '),
        "&mdash;" => Some('\u{2014}'),
        "&ndash;" => Some('\u{2013}'),
        "&hellip;" => Some('\u{2026}'),
        "&middot;" => Some('\u{00b7}'),
        "&ldquo;" => Some('\u{201c}'),
        "&rdquo;" => Some('\u{201d}'),
        "&lsquo;" => Some('\u{2018}'),
        "&rsquo;" => Some('\u{2019}'),
        "&copy;" => Some('\u{00a9}'),
        "&reg;" => Some('\u{00ae}'),
        "&trade;" => Some('\u{2122}'),
        _ => return decode_numeric_entity(entity),
    };
    known
}

fn decode_numeric_entity(entity: &str) -> Option<char> {
    let body = entity.strip_prefix("&#")?.strip_suffix(';')?;
    let code = match body.strip_prefix(['x', 'X']) {
        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
        None => body.parse::<u32>().ok()?,
    };
    char::from_u32(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_markup_scripts_styles_and_decodes_entities() {
        let html = "<!DOCTYPE html><html><head><title>方案 v0.1</title>\
<style>:root{--ink:#1c1f23}</style><script>var x = 1 < 2;</script></head>\
<body><h1>智能座舱方案</h1><p>核心结论：内存 32G &amp; 算力 20 TOPS。</p>\
<!-- hidden note --></body></html>";
        assert_eq!(
            html_to_plain_text(html),
            "方案 v0.1 智能座舱方案 核心结论：内存 32G & 算力 20 TOPS。"
        );
    }

    #[test]
    fn decodes_numeric_and_nbsp_entities() {
        assert_eq!(html_to_plain_text("A&nbsp;B&#44;&#x20;C"), "A B, C");
    }

    #[test]
    fn leaves_plain_text_untouched() {
        assert_eq!(html_to_plain_text("plain  text\nline"), "plain text line");
    }
}
