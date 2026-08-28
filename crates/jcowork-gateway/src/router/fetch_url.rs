//! URL fetching endpoint with HTML-to-text conversion.

use axum::{http::StatusCode, response::IntoResponse, Json};
use serde::Deserialize;

use super::AuthUser;

#[derive(Debug, Deserialize)]
pub(crate) struct FetchUrlRequest {
    url: String,
}

/// Convert HTML to plain text by stripping tags and decoding entities.
/// Good enough for LLM context - no external HTML parser dependency needed.
fn html_to_text(html: &str) -> String {
    let mut text = html.to_string();

    // Remove script and style blocks (including content)
    for tag in ["script", "style", "noscript", "head"] {
        let open = format!("<{}", tag);
        let close = format!("</{}>", tag);
        while let Some(start) = text.to_lowercase().find(&open) {
            if let Some(end) = text.to_lowercase()[start..].find(&close) {
                let abs_end = start + end + close.len();
                text.replace_range(start..abs_end, " ");
            } else {
                // No closing tag - remove to end of open tag
                if let Some(gt) = text[start..].find('>') {
                    text.replace_range(start..start + gt + 1, " ");
                } else {
                    break;
                }
            }
        }
    }

    // Replace <br>, <p>, <div>, <li> tags with newlines
    for tag in ["<br", "<br/", "<br /", "<p", "</p>", "<div", "</div>", "<li", "</li>", "<h1", "<h2", "<h3", "<h4", "<h5", "<h6", "</h1>", "</h2>", "</h3>", "</h4>", "</h5>", "</h6>", "<tr", "</tr>"] {
        let replacement = if tag.starts_with("</") { "\n" } else { "\n" };
        text = text.replace(tag, replacement);
    }

    // Strip all remaining HTML tags
    let mut result = String::with_capacity(text.len());
    let mut in_tag = false;
    for ch in text.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => result.push(ch),
            _ => {}
        }
    }

    // Decode common HTML entities
    let result = result
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
        .replace("&#x27;", "'")
        .replace("&mdash;", "\u{2014}")
        .replace("&ndash;", "\u{2013}")
        .replace("&hellip;", "\u{2026}")
        .replace("&copy;", "\u{00A9}")
        .replace("&reg;", "\u{00AE}");

    // Collapse multiple whitespace/newlines
    let mut cleaned = String::with_capacity(result.len());
    let mut prev_was_space = false;
    for line in result.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if !prev_was_space {
                cleaned.push('\n');
                prev_was_space = true;
            }
        } else {
            // Collapse internal whitespace
            let collapsed: String = trimmed.split_whitespace().collect::<Vec<_>>().join(" ");
            cleaned.push_str(&collapsed);
            cleaned.push('\n');
            prev_was_space = false;
        }
    }

    // Truncate to 100KB to avoid token overflow
    let mut final_text = cleaned.trim().to_string();
    if final_text.len() > 100 * 1024 {
        final_text.truncate(100 * 1024);
        final_text.push_str("\n\n[... CONTENT TRUNCATED: page exceeds 100KB ...]");
    }
    final_text
}

/// Extract the page title from HTML.
fn extract_title(html: &str) -> String {
    let lower = html.to_lowercase();
    if let Some(start) = lower.find("<title") {
        if let Some(gt) = lower[start..].find('>') {
            let content_start = start + gt + 1;
            if let Some(end_tag) = lower[content_start..].find("</title>") {
                return html[content_start..content_start + end_tag].trim().to_string();
            }
        }
    }
    "Untitled".to_string()
}

pub(crate) async fn fetch_url(
    axum::Extension(_auth_user): axum::Extension<AuthUser>,
    Json(req): Json<FetchUrlRequest>,
) -> impl IntoResponse {
    let url = req.url.trim();

    // Basic URL validation
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "URL must start with http:// or https://" })),
        ).into_response();
    }

    // Fetch the page
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::limited(5))
        .user_agent("Mozilla/5.0 (compatible; JcoworkBot/1.0)")
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("Failed to create HTTP client: {}", e) })),
            ).into_response();
        }
    };

    let resp = match client.get(url).send().await {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({ "error": format!("Failed to fetch URL: {}", e) })),
            ).into_response();
        }
    };

    let status = resp.status();
    if !status.is_success() {
        return (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({ "error": format!("HTTP {} from {}", status, url) })),
        ).into_response();
    }

    let html = match resp.text().await {
        Ok(t) => t,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({ "error": format!("Failed to read response: {}", e) })),
            ).into_response();
        }
    };

    let title = extract_title(&html);
    let text = html_to_text(&html);

    Json(serde_json::json!({
        "url": url,
        "title": title,
        "text": text,
    })).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_title_basic() {
        assert_eq!(extract_title("<html><head><title>Hello World</title></head></html>"), "Hello World");
    }

    #[test]
    fn test_extract_title_with_attributes() {
        assert_eq!(extract_title("<TITLE lang=\"en\">  My Page  </TITLE>"), "My Page");
    }

    #[test]
    fn test_extract_title_missing() {
        assert_eq!(extract_title("<html><body>no title here</body></html>"), "Untitled");
    }

    #[test]
    fn test_html_to_text_strips_tags() {
        let text = html_to_text("<p>Hello <b>world</b></p>");
        assert!(text.contains("Hello world"), "got: {:?}", text);
        assert!(!text.contains('<'));
    }

    #[test]
    fn test_html_to_text_removes_script_and_style() {
        let html = "<style>.a{color:red}</style><script>alert('x');</script><p>visible</p>";
        let text = html_to_text(html);
        assert!(text.contains("visible"));
        assert!(!text.contains("alert"));
        assert!(!text.contains("color:red"));
    }

    #[test]
    fn test_html_to_text_decodes_entities() {
        let text = html_to_text("<p>a &amp; b &lt;c&gt; &quot;d&quot; &nbsp;e</p>");
        assert!(text.contains("a & b <c> \"d\""), "got: {:?}", text);
    }

    #[test]
    fn test_html_to_text_collapses_blank_lines() {
        let text = html_to_text("<p>one</p><br><br><br><br><p>two</p>");
        assert!(!text.contains("\n\n\n"), "too many blank lines: {:?}", text);
    }

    #[test]
    fn test_html_to_text_empty() {
        assert_eq!(html_to_text(""), "");
    }
}
