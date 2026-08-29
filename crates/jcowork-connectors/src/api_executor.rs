//! HTTP executor for API connector tools.
//!
//! Renders `{{param}}` placeholders in the URL (and optional body template)
//! from the arguments provided by the LLM, issues the HTTP request, and
//! returns the response body as a truncated string.

use anyhow::{bail, Result};
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;

use crate::models::ApiToolDef;

/// Request timeout for API tool calls.
pub const API_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Maximum response body size returned to the model.
pub const MAX_RESPONSE_BYTES: usize = 200 * 1024;

/// Parse the LLM-provided arguments JSON string into an object.
pub fn parse_args(args: &str) -> Result<HashMap<String, String>> {
    let trimmed = args.trim();
    if trimmed.is_empty() {
        return Ok(HashMap::new());
    }
    let value: Value = serde_json::from_str(trimmed)
        .map_err(|e| anyhow::anyhow!("Invalid tool arguments JSON: {}", e))?;
    let obj = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("Tool arguments must be a JSON object"))?;
    Ok(obj
        .iter()
        .map(|(k, v)| {
            let s = match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            (k.clone(), s)
        })
        .collect())
}

/// Render `{{param}}` placeholders in a template string.
///
/// Returns an error naming the first missing placeholder so the model gets
/// actionable feedback instead of a malformed request.
pub fn render_template(template: &str, args: &HashMap<String, String>) -> Result<String> {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after.find("}}") {
            Some(end) => {
                let key = after[..end].trim();
                match args.get(key) {
                    Some(v) => out.push_str(v),
                    None => bail!("Missing required parameter: {}", key),
                }
                rest = &after[end + 2..];
            }
            None => {
                // Unclosed placeholder: keep the literal text.
                out.push_str(&rest[start..]);
                return Ok(out);
            }
        }
    }
    out.push_str(rest);
    Ok(out)
}

/// Collect the placeholder names referenced by a template.
pub fn template_placeholders(template: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        match after.find("}}") {
            Some(end) => {
                names.push(after[..end].trim().to_string());
                rest = &after[end + 2..];
            }
            None => break,
        }
    }
    names
}

/// Truncate a response body to MAX_RESPONSE_BYTES without splitting UTF-8.
pub fn truncate_response(body: &str) -> String {
    if body.len() <= MAX_RESPONSE_BYTES {
        return body.to_string();
    }
    let mut end = MAX_RESPONSE_BYTES;
    while end > 0 && !body.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n... [truncated]", &body[..end])
}

/// Execute an API tool: render templates, send the HTTP request, return body.
pub async fn execute_api_tool(tool: &ApiToolDef, args_json: &str) -> Result<String> {
    let args = parse_args(args_json)?;
    let url = render_template(&tool.url, &args)?;

    let client = reqwest::Client::builder()
        .timeout(API_REQUEST_TIMEOUT)
        .build()?;
    let method = tool.method.to_uppercase();
    let mut req = match method.as_str() {
        "GET" => client.get(&url),
        "POST" => client.post(&url),
        "PUT" => client.put(&url),
        "PATCH" => client.patch(&url),
        "DELETE" => client.delete(&url),
        other => bail!("Unsupported HTTP method: {}", other),
    };
    for (k, v) in &tool.headers {
        req = req.header(k.as_str(), v.as_str());
    }

    let has_body = matches!(method.as_str(), "POST" | "PUT" | "PATCH");
    if has_body {
        let (body, is_json) = match &tool.body_template {
            Some(tpl) => (render_template(tpl, &args)?, false),
            // No template: send the whole args object as JSON.
            None => {
                let obj: Value = serde_json::from_str(if args_json.trim().is_empty() {
                    "{}"
                } else {
                    args_json
                })?;
                (obj.to_string(), true)
            }
        };
        if is_json {
            req = req.header("Content-Type", "application/json");
        }
        req = req.body(body);
    }

    let resp = req
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Request to {} failed: {}", url, e))?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Ok(format!(
            "HTTP {} {}\n{}",
            status.as_u16(),
            status.canonical_reason().unwrap_or(""),
            truncate_response(&body)
        ));
    }
    Ok(truncate_response(&body))
}

/// Validate an API tool definition without issuing any request.
///
/// Checks that every placeholder in the URL/body template is declared in the
/// tool's parameter schema, so misconfigurations surface at save time.
pub fn validate_api_tool(tool: &ApiToolDef) -> Result<()> {
    if tool.name.trim().is_empty() {
        bail!("Tool name must not be empty");
    }
    if !tool.url.starts_with("http://") && !tool.url.starts_with("https://") {
        bail!("Tool URL must start with http:// or https://");
    }
    let declared: Vec<String> = tool
        .params
        .get("properties")
        .and_then(|p| p.as_object())
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();

    let mut placeholders = template_placeholders(&tool.url);
    if let Some(tpl) = &tool.body_template {
        placeholders.extend(template_placeholders(tpl));
    }
    for name in &placeholders {
        if !declared.iter().any(|d| d == name) {
            bail!(
                "Placeholder '{{{{{}}}}}' is not declared in the parameter schema",
                name
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_tool(url: &str, params: Value, body_template: Option<String>) -> ApiToolDef {
        ApiToolDef {
            name: "t".to_string(),
            description: "d".to_string(),
            method: "GET".to_string(),
            url: url.to_string(),
            headers: HashMap::new(),
            params,
            body_template,
            enabled: true,
        }
    }

    #[test]
    fn test_render_template_basic() {
        let mut args = HashMap::new();
        args.insert("city".to_string(), "beijing".to_string());
        args.insert("day".to_string(), "3".to_string());
        let out = render_template("https://x.com/w?city={{city}}&d={{day}}", &args).unwrap();
        assert_eq!(out, "https://x.com/w?city=beijing&d=3");
    }

    #[test]
    fn test_render_template_missing_param() {
        let args = HashMap::new();
        let err = render_template("https://x.com/{{q}}", &args).unwrap_err();
        assert!(err.to_string().contains("q"));
    }

    #[test]
    fn test_render_template_no_placeholders() {
        let args = HashMap::new();
        assert_eq!(
            render_template("https://x.com/plain", &args).unwrap(),
            "https://x.com/plain"
        );
    }

    #[test]
    fn test_template_placeholders() {
        let names = template_placeholders("{{a}}/x/{{ b }}/{{c}}");
        assert_eq!(names, vec!["a", "b", "c"]);
        assert!(template_placeholders("no placeholders").is_empty());
    }

    #[test]
    fn test_parse_args_variants() {
        let args = parse_args(r#"{"city": "sh", "n": 5, "flag": true}"#).unwrap();
        assert_eq!(args.get("city").unwrap(), "sh");
        assert_eq!(args.get("n").unwrap(), "5");
        assert_eq!(args.get("flag").unwrap(), "true");
        assert!(parse_args("").unwrap().is_empty());
        assert!(parse_args("not json").is_err());
        assert!(parse_args("[1,2]").is_err());
    }

    #[test]
    fn test_truncate_response() {
        assert_eq!(truncate_response("short"), "short");
        let big = "x".repeat(MAX_RESPONSE_BYTES + 100);
        let out = truncate_response(&big);
        assert!(out.ends_with("... [truncated]"));
        assert!(out.len() <= MAX_RESPONSE_BYTES + 32);

        // UTF-8 boundary: truncation must not split a multi-byte char
        let cn = "测".repeat(MAX_RESPONSE_BYTES); // each char 3 bytes
        let out = truncate_response(&cn);
        assert!(out.starts_with("测"));
        assert!(out.ends_with("... [truncated]"));
    }

    #[test]
    fn test_validate_api_tool_ok() {
        let tool = make_tool(
            "https://x.com/w?city={{city}}",
            json!({"type":"object","properties":{"city":{"type":"string"}},"required":["city"]}),
            None,
        );
        assert!(validate_api_tool(&tool).is_ok());
    }

    #[test]
    fn test_validate_api_tool_undeclared_placeholder() {
        let tool = make_tool("https://x.com/w?city={{city}}", json!({"type":"object"}), None);
        let err = validate_api_tool(&tool).unwrap_err();
        assert!(err.to_string().contains("city"));
    }

    #[test]
    fn test_validate_api_tool_bad_url_and_empty_name() {
        let mut tool = make_tool("ftp://x", json!({"type":"object"}), None);
        assert!(validate_api_tool(&tool).is_err());
        tool.url = "https://x.com".to_string();
        tool.name = "  ".to_string();
        assert!(validate_api_tool(&tool).is_err());
    }

    #[test]
    fn test_validate_api_tool_body_template_placeholder() {
        let tool = make_tool(
            "https://x.com/post",
            json!({"type":"object","properties":{"q":{"type":"string"}}}),
            Some(r#"{"query":"{{missing}}"}"#.to_string()),
        );
        let err = validate_api_tool(&tool).unwrap_err();
        assert!(err.to_string().contains("missing"));
    }
}
