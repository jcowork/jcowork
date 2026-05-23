//! report_search tool — queries the jcowork-report-search service.
//!
//! The report search service (port 3001) must be running. It indexes PDFs
//! from ~/.jcowork/data/reports/ automatically. This tool lets the agent
//! search the indexed documents by keyword.

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::base::{Tool, ToolContext};

/// Default URL of the report search service.
const REPORT_SEARCH_URL: &str = "http://localhost:3001";

/// report_search tool: queries the jcowork-report-search service.
pub struct ReportSearchTool {
    base_url: String,
}

impl Default for ReportSearchTool {
    fn default() -> Self {
        Self {
            base_url: std::env::var("JCWORK_REPORT_SEARCH_URL")
                .unwrap_or_else(|_| REPORT_SEARCH_URL.to_string()),
        }
    }
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct SearchArgs {
    query: String,
    company: Option<String>,
    doc_type: Option<String>,
    limit: Option<u32>,
}

#[derive(Deserialize, Serialize)]
struct SearchResult {
    doc_id: String,
    company: String,
    filename: String,
    doc_type: String,
    year: Option<i64>,
    chunk: String,
    score: f64,
}

#[async_trait]
impl Tool for ReportSearchTool {
    fn name(&self) -> &str {
        "report_search"
    }

    fn description(&self) -> &str {
        "Search indexed company reports (annual reports, quarterly reports, broker research) by keyword. \
         Returns relevant text chunks from matching documents. \
         The report search service automatically indexes PDFs placed in ~/.jcowork/data/reports/{company_name}/. \
         Use multiple targeted queries to gather different aspects of a company's financials."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search keywords or phrase (Chinese or English). e.g. '营业收入 净利润', '主营业务 竞争壁垒', '风险因素'"
                },
                "company": {
                    "type": "string",
                    "description": "Filter by company name, e.g. '瑞晟智能'. If omitted, searches all indexed companies."
                },
                "doc_type": {
                    "type": "string",
                    "description": "Filter by document type: '年报' | '季报' | '研报' | '招股书'. If omitted, searches all types."
                },
                "limit": {
                    "type": "integer",
                    "description": "Max number of chunks to return (default 15, max 50)."
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(&self, args: &str, _ctx: &ToolContext) -> Result<String> {
        let parsed: serde_json::Value = serde_json::from_str(args)?;
        let query = parsed["query"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'query' parameter"))?;

        let company = parsed["company"].as_str();
        let doc_type = parsed["doc_type"].as_str();
        let limit = parsed["limit"].as_u64().unwrap_or(15).min(50);

        // Build request URL
        let mut url = format!("{}/search?q={}&limit={}", self.base_url, urlencoding(query), limit);
        if let Some(c) = company {
            url.push_str(&format!("&company={}", urlencoding(c)));
        }
        if let Some(dt) = doc_type {
            url.push_str(&format!("&doc_type={}", urlencoding(dt)));
        }

        // Make HTTP request
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()?;

        let response = match client.get(&url).send().await {
            Ok(r) => r,
            Err(e) => {
                return Ok(format!(
                    "Report search service is not available ({}). \
                     Please start it with: cargo run --bin jcowork-report-search\n\
                     Error: {}",
                    self.base_url, e
                ));
            }
        };

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Ok(format!("Report search returned error {}: {}", status, body));
        }

        let results: Vec<SearchResult> = response.json().await?;

        if results.is_empty() {
            return Ok(format!(
                "No results found for query: \"{}\". \
                 Try broader keywords or check if documents are indexed at /health endpoint.",
                query
            ));
        }

        // Format results as readable text for the LLM
        let mut output = format!(
            "Found {} result(s) for query: \"{}\"\n\n",
            results.len(),
            query
        );

        for (i, r) in results.iter().enumerate() {
            let year_str = r.year.map(|y| y.to_string()).unwrap_or_else(|| "?".to_string());
            output.push_str(&format!(
                "--- Result {} ---\n[Source: {} | {} | {} | score: {:.2}]\n{}\n\n",
                i + 1,
                r.filename,
                r.doc_type,
                year_str,
                r.score,
                r.chunk.trim()
            ));
        }

        Ok(output)
    }
}

/// Simple percent-encoding for URL query parameters.
fn urlencoding(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9'
            | b'-' | b'_' | b'.' | b'~' => out.push(byte as char),
            b' ' => out.push('+'),
            b => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

/// report_list_companies tool — lists all indexed companies.
pub struct ReportListCompaniesTool {
    base_url: String,
}

impl Default for ReportListCompaniesTool {
    fn default() -> Self {
        Self {
            base_url: std::env::var("JCWORK_REPORT_SEARCH_URL")
                .unwrap_or_else(|_| REPORT_SEARCH_URL.to_string()),
        }
    }
}

#[async_trait]
impl Tool for ReportListCompaniesTool {
    fn name(&self) -> &str {
        "report_list_companies"
    }

    fn description(&self) -> &str {
        "List all companies whose reports are indexed in the report search service. \
         Use this to discover available companies before searching."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {}
        })
    }

    async fn execute(&self, _args: &str, _ctx: &ToolContext) -> Result<String> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()?;

        let health_url = format!("{}/health", self.base_url);
        let companies_url = format!("{}/companies", self.base_url);

        // First check health
        let health_resp = match client.get(&health_url).send().await {
            Ok(r) => r,
            Err(e) => {
                return Ok(format!(
                    "Report search service is not available. Start with: cargo run --bin jcowork-report-search\nError: {}",
                    e
                ));
            }
        };

        let health: serde_json::Value = health_resp.json().await.unwrap_or_default();
        let doc_count = health["indexed_documents"].as_i64().unwrap_or(0);
        let chunk_count = health["indexed_chunks"].as_i64().unwrap_or(0);

        // Get company list
        let companies_resp = client.get(&companies_url).send().await?;
        let companies: Vec<String> = companies_resp.json().await.unwrap_or_default();

        if companies.is_empty() {
            return Ok(format!(
                "No companies indexed yet. Total: {} documents, {} chunks.\n\
                 Place PDF files in ~/.jcowork/data/reports/{{company_name}}/ to index them.",
                doc_count, chunk_count
            ));
        }

        let list = companies.join(", ");
        Ok(format!(
            "Indexed companies ({} docs, {} chunks): {}\n\
             Use report_search with company filter to query specific companies.",
            doc_count, chunk_count, list
        ))
    }
}
