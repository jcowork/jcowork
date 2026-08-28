//! PDF parsing tool - uses pdftext (CPU-only, no AI model download required) to parse
//! PDF files into LLM-friendly plain text. Supports single files or directories.
//!
//! pdftext is from the Surya OCR team.
//! It works entirely offline using pypdfium2 for layout analysis.

use anyhow::Result;
use async_trait::async_trait;
use std::path::Path;
use tokio::process::Command;
use tokio::time::{timeout, Duration};

use crate::base::{Tool, ToolContext};

/// Maximum output size (200KB). Parsed PDFs can be very large; truncation prevents token overflow.
const MAX_OUTPUT_BYTES: usize = 200 * 1024;

/// Resolve the Python binary path in the jcowork venv.
/// On Unix: ~/.jcowork/venv/bin/python
/// On Windows: ~/.jcowork/venv/Scripts/python.exe
/// Uses HOME with USERPROFILE fallback — on Windows the desktop app process
/// may not have HOME set, so shellexpand::tilde cannot be relied upon.
fn resolve_python_bin() -> String {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_default();
    if cfg!(windows) {
        format!("{}\\.jcowork\\venv\\Scripts\\python.exe", home)
    } else {
        format!("{}/.jcowork/venv/bin/python", home)
    }
}

/// Inline Python script for PDF text extraction using pdftext.
/// pdftext uses pypdfium2 for layout analysis and works fully offline.
const PDF_PARSE_SCRIPT: &str = r#"
import sys
import os

path = sys.argv[1]

# Collect PDF files
pdf_files = []
if os.path.isfile(path):
    if path.lower().endswith('.pdf'):
        pdf_files.append(path)
    else:
        print(f"Warning: {path} is not a PDF file", file=sys.stderr)
elif os.path.isdir(path):
    for root, dirs, files in os.walk(path):
        for f in sorted(files):
            if f.lower().endswith('.pdf'):
                pdf_files.append(os.path.join(root, f))
else:
    print(f"Error: path does not exist: {path}", file=sys.stderr)
    sys.exit(1)

if not pdf_files:
    print("No PDF files found at the specified path.", file=sys.stderr)
    sys.exit(1)

from pdftext.extraction import plain_text_output

results = []
for pdf_path in pdf_files:
    try:
        text = plain_text_output(pdf_path)
        header = f"\n\n{'='*60}\nSource: {os.path.basename(pdf_path)}\n{'='*60}\n\n"
        results.append(header + text)
    except Exception as e:
        results.append(f"\n[Error parsing {os.path.basename(pdf_path)}: {e}]")

print(''.join(results))
"#;

/// PDF parsing tool that uses pdftext to convert PDFs into LLM-friendly plain text.
///
/// Supports single PDF files or directories containing multiple PDFs.
/// Works fully offline (no AI model download required), using pypdfium2 for layout analysis.
/// For best results with Chinese financial reports, it preserves table structure as plain text.
pub struct PdfParseTool {
    timeout_secs: u64,
}

impl PdfParseTool {
    pub fn new(timeout_secs: u64) -> Self {
        Self { timeout_secs }
    }
}

impl Default for PdfParseTool {
    fn default() -> Self {
        Self::new(180)
    }
}

#[async_trait]
impl Tool for PdfParseTool {
    fn name(&self) -> &str {
        "pdf_parse"
    }

    fn description(&self) -> &str {
        "Parse PDF files into LLM-friendly plain text. Supports single PDF files or directories containing multiple PDFs. Works offline with no AI model download required. Ideal for reading quarterly/annual reports, research reports, and other PDF documents. Company reports are stored at ~/.jcowork/data/reports/{company_name}/"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Absolute path to a PDF file or directory containing PDF files. Tilde (~) is supported. For company reports use ~/.jcowork/data/reports/{company_name}/"
                }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, args: &str, _ctx: &ToolContext) -> Result<String> {
        let parsed: serde_json::Value = serde_json::from_str(args)?;
        let path = parsed["path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'path' parameter"))?;

        // Expand tilde
        let expanded_path = shellexpand::tilde(path).to_string();

        // Verify path exists
        if !Path::new(&expanded_path).exists() {
            anyhow::bail!("Path does not exist: {}", expanded_path);
        }

        // Resolve python binary path
        let python_bin = resolve_python_bin();

        if !Path::new(&python_bin).exists() {
            anyhow::bail!(
                "Python not found at {}. Run: scripts/setup-python.sh (or setup-python.ps1 on Windows)",
                python_bin
            );
        }

        // Run pdftext via Python in venv
        let result = timeout(
            Duration::from_secs(self.timeout_secs),
            Command::new(&python_bin)
                .arg("-c")
                .arg(PDF_PARSE_SCRIPT)
                .arg(&expanded_path)
                .output(),
        )
        .await;

        let output = match result {
            Ok(Ok(o)) => o,
            Ok(Err(e)) => anyhow::bail!("Failed to run PDF parser: {}", e),
            Err(_) => anyhow::bail!(
                "PDF parsing timed out after {}s for: {}",
                self.timeout_secs,
                expanded_path
            ),
        };

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("PDF parser failed: {}", stderr);
        }

        let mut text = String::from_utf8_lossy(&output.stdout).into_owned();

        if text.trim().is_empty() {
            return Ok("No text extracted. The PDF may be empty, scanned image-only, or corrupted.".to_string());
        }

        // Truncate if too large to avoid token overflow
        if text.len() > MAX_OUTPUT_BYTES {
            text.truncate(MAX_OUTPUT_BYTES);
            text.push_str("\n\n[... OUTPUT TRUNCATED: document exceeds 200KB. Consider parsing specific pages or a single file at a time.]");
        }

        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_python_bin_uses_home_not_tilde() {
        // The old shellexpand::tilde approach returned an unusable path when
        // HOME was unset (Windows desktop app process). The resolved path must
        // never contain an unexpanded tilde and must point into the venv.
        let bin = resolve_python_bin();
        assert!(!bin.contains('~'), "unexpanded tilde in python path: {}", bin);

        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_default();
        assert!(
            bin.starts_with(&home),
            "python path {} should be rooted in home dir {}",
            bin,
            home
        );
        if cfg!(windows) {
            assert!(bin.ends_with(".jcowork\\venv\\Scripts\\python.exe"), "{}", bin);
        } else {
            assert!(bin.ends_with(".jcowork/venv/bin/python"), "{}", bin);
        }
    }
}
