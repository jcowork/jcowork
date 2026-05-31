//! PDF parsing and text chunking.
//!
//! Uses pdftext (via Python subprocess) to extract text from PDFs.
//! Splits text into overlapping chunks for FTS indexing.

use anyhow::Result;
use tokio::process::Command;
use tokio::time::{timeout, Duration};

/// Python binary inside the jcowork venv (has pdftext pre-installed).
const PYTHON_BIN: &str = "~/.jcowork/venv/bin/python";

/// Chunk size in characters.
pub const CHUNK_SIZE: usize = 1000;
/// Overlap between consecutive chunks in characters.
pub const CHUNK_OVERLAP: usize = 100;

/// Python script that uses pdftext to extract plain text from a PDF.
const EXTRACT_SCRIPT: &str = r#"
import sys
from pdftext.extraction import plain_text_output

path = sys.argv[1]
try:
    text = plain_text_output(path)
    print(text, end='')
except Exception as e:
    print(f"ERROR: {e}", file=sys.stderr)
    sys.exit(1)
"#;

/// Extract plain text from a PDF file using pdftext.
/// Returns the full extracted text string.
pub async fn extract_text(pdf_path: &str) -> Result<String> {
    let python_bin = shellexpand::tilde(PYTHON_BIN).to_string();

    let result = timeout(
        Duration::from_secs(300), // 5 min max for large PDFs
        Command::new(&python_bin)
            .arg("-c")
            .arg(EXTRACT_SCRIPT)
            .arg(pdf_path)
            .output(),
    )
    .await;

    let output = match result {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => anyhow::bail!("Failed to spawn pdftext: {}", e),
        Err(_) => anyhow::bail!("pdftext timed out after 300s for: {}", pdf_path),
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("pdftext failed for {}: {}", pdf_path, stderr);
    }

    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    if text.trim().is_empty() {
        return Ok(String::new()); // Image-only / scanned PDF — caller decides what to do
    }

    Ok(text)
}

/// Split text into overlapping chunks of approximately CHUNK_SIZE chars.
/// Tries to split on newline boundaries to preserve paragraph structure.
pub fn split_into_chunks(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let total = chars.len();
    if total == 0 {
        return vec![];
    }

    let mut chunks = Vec::new();
    let mut start = 0;

    while start < total {
        let end = (start + CHUNK_SIZE).min(total);

        // Try to find a clean split point (newline) near the end
        let split_at = if end < total {
            // Look for the last newline within the last 20% of the chunk
            let search_start = start + (CHUNK_SIZE * 4 / 5);
            // Count newline position in chars (not bytes)
            let nl_char_offset = chars[search_start..end]
                .iter()
                .rposition(|&c| c == '\n');
            if let Some(nl_pos) = nl_char_offset {
                (search_start + nl_pos + 1).min(total)
            } else {
                end
            }
        } else {
            end
        };

        let chunk: String = chars[start..split_at].iter().collect();
        let trimmed = chunk.trim().to_string();
        if !trimmed.is_empty() {
            chunks.push(trimmed);
        }

        // Next chunk starts at split_at minus overlap
        if split_at >= total {
            break;
        }
        start = split_at.saturating_sub(CHUNK_OVERLAP);
    }

    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_split_empty() {
        assert!(split_into_chunks("").is_empty());
    }

    #[test]
    fn test_split_short() {
        let chunks = split_into_chunks("hello world");
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], "hello world");
    }

    #[test]
    fn test_split_long() {
        let text = "a".repeat(3500);
        let chunks = split_into_chunks(&text);
        // Should produce multiple chunks
        assert!(chunks.len() > 2);
        // Each chunk should be within size range
        for c in &chunks {
            assert!(c.chars().count() <= CHUNK_SIZE + 50);
        }
    }
}
