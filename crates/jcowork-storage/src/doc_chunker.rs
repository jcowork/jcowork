//! Markdown document chunker for vector indexing.
//!
//! Splits structured Markdown documents (from Docling) into semantic chunks:
//! - Text sections (split by headings)
//! - Tables (each table is a separate chunk)
//! - Images (each image reference is a separate chunk)

/// A chunk of a document, ready for embedding and indexing.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DocChunk {
    /// The file path this chunk belongs to
    pub file_path: String,
    /// Type of chunk: "text", "table", or "image"
    pub chunk_type: String,
    /// The content of the chunk (Markdown format)
    pub content: String,
    /// The heading context (parent section title)
    pub heading: String,
    /// Index of this chunk within the document (0-based)
    pub chunk_index: i32,
    /// Image file path (only for "image" type chunks)
    pub image_path: Option<String>,
}

/// Maximum characters per text chunk before splitting
const MAX_CHUNK_SIZE: usize = 10000;

/// Minimum characters for a meaningful chunk
const MIN_CHUNK_SIZE: usize = 30;

/// Chunk a Markdown document into semantic units.
///
/// The chunker splits by:
/// 1. Headings (#, ##, ###, ####) for section boundaries
/// 2. Tables (each table becomes its own chunk)
/// 3. Images (each image becomes its own chunk)
///
/// Strategy for completeness:
/// - Short sections (poems, short articles) under SOFT_CHUNK_SIZE are kept intact
/// - Long sections are split at paragraph boundaries (double newlines)
/// - Very long paragraphs are split at sentence boundaries (。！？.!?)
/// - Each chunk carries its parent heading for context
pub fn chunk_markdown(markdown: &str, file_path: &str) -> Vec<DocChunk> {
    let mut chunks = Vec::new();
    let mut chunk_index = 0;
    
    let mut current_heading = String::new();
    let mut current_text = String::new();
    let mut in_table = false;
    let mut table_lines: Vec<String> = Vec::new();
    
    for line in markdown.lines() {
        let trimmed = line.trim();
        
        // Check for heading (#, ##, ###, ####)
        if trimmed.starts_with("# ") || trimmed.starts_with("## ") || trimmed.starts_with("### ") || trimmed.starts_with("#### ") {
            // Flush current text section
            if !current_text.trim().is_empty() {
                flush_text_chunks(
                    &mut chunks,
                    file_path,
                    &current_heading,
                    &current_text,
                    &mut chunk_index,
                );
                current_text.clear();
            }
            
            // Exit table mode if we were in one
            if in_table {
                flush_table_chunk(&mut chunks, file_path, &current_heading, &table_lines, &mut chunk_index);
                table_lines.clear();
                in_table = false;
            }
            
            // Update current heading
            current_heading = trimmed.trim_start_matches('#').trim().to_string();
            continue;
        }
        
        // Check for table start (line with |)
        if trimmed.starts_with('|') && trimmed.ends_with('|') {
            // Flush text before table
            if !current_text.trim().is_empty() {
                flush_text_chunks(
                    &mut chunks,
                    file_path,
                    &current_heading,
                    &current_text,
                    &mut chunk_index,
                );
                current_text.clear();
            }
            
            in_table = true;
            table_lines.push(line.to_string());
            continue;
        }
        
        // If we were in a table and this line is not a table row, flush the table
        if in_table {
            flush_table_chunk(&mut chunks, file_path, &current_heading, &table_lines, &mut chunk_index);
            table_lines.clear();
            in_table = false;
        }
        
        // Check for image
        if trimmed.starts_with("![") {
            // Flush current text
            if !current_text.trim().is_empty() {
                flush_text_chunks(
                    &mut chunks,
                    file_path,
                    &current_heading,
                    &current_text,
                    &mut chunk_index,
                );
                current_text.clear();
            }
            
            // Parse image: ![alt](path)
            if let Some((alt, path)) = parse_image(trimmed) {
                chunks.push(DocChunk {
                    file_path: file_path.to_string(),
                    chunk_type: "image".to_string(),
                    content: format!("Image: {}. Path: {}", alt, path),
                    heading: current_heading.clone(),
                    chunk_index,
                    image_path: Some(path),
                });
                chunk_index += 1;
            }
            continue;
        }
        
        // Regular text line - accumulate
        current_text.push_str(line);
        current_text.push('\n');
    }
    
    // Flush remaining content
    if in_table && !table_lines.is_empty() {
        flush_table_chunk(&mut chunks, file_path, &current_heading, &table_lines, &mut chunk_index);
    }
    
    if !current_text.trim().is_empty() {
        flush_text_chunks(
            &mut chunks,
            file_path,
            &current_heading,
            &current_text,
            &mut chunk_index,
        );
    }
    
    chunks
}

/// Parse a Markdown image: ![alt](path)
fn parse_image(line: &str) -> Option<(String, String)> {
    let line = line.trim();
    if !line.starts_with("![") {
        return None;
    }
    
    let after_bracket = line.find("](")?;
    let alt = line[2..after_bracket].to_string();
    
    let path_start = after_bracket + 2;
    let path_end = line[path_start..].find(')')? + path_start;
    let path = line[path_start..path_end].to_string();
    
    Some((alt, path))
}

/// Flush accumulated text into one or more chunks.
/// Strategy:
/// - If text fits within MAX_CHUNK_SIZE, keep it as one chunk (preserves poems/short articles)
/// - If text exceeds MAX_CHUNK_SIZE, split at paragraph boundaries (\n\n)
/// - If a single paragraph exceeds MAX_CHUNK_SIZE, split at sentence boundaries (。！？.!?)
fn flush_text_chunks(
    chunks: &mut Vec<DocChunk>,
    file_path: &str,
    heading: &str,
    text: &str,
    chunk_index: &mut i32,
) {
    let text = text.trim();
    if text.len() < MIN_CHUNK_SIZE {
        return;
    }
    
    if text.len() <= MAX_CHUNK_SIZE {
        // Keep as one chunk - preserves完整性 for poems, short articles, etc.
        chunks.push(DocChunk {
            file_path: file_path.to_string(),
            chunk_type: "text".to_string(),
            content: text.to_string(),
            heading: heading.to_string(),
            chunk_index: *chunk_index,
            image_path: None,
        });
        *chunk_index += 1;
        return;
    }
    
    // Text exceeds MAX_CHUNK_SIZE, need to split
    // First try splitting by paragraphs (double newline)
    let paragraphs: Vec<&str> = text.split("\n\n").collect();
    
    // If splitting by paragraphs gives reasonable chunks, use that
    let max_para_len = paragraphs.iter().map(|p| p.len()).max().unwrap_or(0);
    if max_para_len <= MAX_CHUNK_SIZE {
        // Group paragraphs into chunks, respecting SOFT_CHUNK_SIZE as a guide
        // but allowing up to MAX_CHUNK_SIZE
        let mut current_chunk = String::new();
        
        for para in &paragraphs {
            let para = para.trim();
            if para.is_empty() {
                continue;
            }
            
            // If adding this paragraph would exceed MAX_CHUNK_SIZE, flush current chunk
            if !current_chunk.is_empty() && current_chunk.len() + para.len() + 2 > MAX_CHUNK_SIZE {
                if current_chunk.trim().len() >= MIN_CHUNK_SIZE {
                    chunks.push(DocChunk {
                        file_path: file_path.to_string(),
                        chunk_type: "text".to_string(),
                        content: current_chunk.trim().to_string(),
                        heading: heading.to_string(),
                        chunk_index: *chunk_index,
                        image_path: None,
                    });
                    *chunk_index += 1;
                }
                current_chunk.clear();
            }
            
            if !current_chunk.is_empty() {
                current_chunk.push_str("\n\n");
            }
            current_chunk.push_str(para);
        }
        
        // Flush remaining
        if current_chunk.trim().len() >= MIN_CHUNK_SIZE {
            chunks.push(DocChunk {
                file_path: file_path.to_string(),
                chunk_type: "text".to_string(),
                content: current_chunk.trim().to_string(),
                heading: heading.to_string(),
                chunk_index: *chunk_index,
                image_path: None,
            });
            *chunk_index += 1;
        }
    } else {
        // Some paragraphs are too long, need sentence-level splitting
        // Split by sentences using Chinese and English sentence endings
        let sentences = split_by_sentences(text);
        let mut current_chunk = String::new();
        
        for sentence in &sentences {
            let sentence = sentence.trim();
            if sentence.is_empty() {
                continue;
            }
            
            // If adding this sentence would exceed MAX_CHUNK_SIZE, flush current chunk
            if !current_chunk.is_empty() && current_chunk.len() + sentence.len() + 1 > MAX_CHUNK_SIZE {
                if current_chunk.trim().len() >= MIN_CHUNK_SIZE {
                    chunks.push(DocChunk {
                        file_path: file_path.to_string(),
                        chunk_type: "text".to_string(),
                        content: current_chunk.trim().to_string(),
                        heading: heading.to_string(),
                        chunk_index: *chunk_index,
                        image_path: None,
                    });
                    *chunk_index += 1;
                }
                current_chunk.clear();
            }
            
            // If a single sentence exceeds MAX_CHUNK_SIZE, force-split it
            if sentence.len() > MAX_CHUNK_SIZE {
                if !current_chunk.is_empty() && current_chunk.trim().len() >= MIN_CHUNK_SIZE {
                    chunks.push(DocChunk {
                        file_path: file_path.to_string(),
                        chunk_type: "text".to_string(),
                        content: current_chunk.trim().to_string(),
                        heading: heading.to_string(),
                        chunk_index: *chunk_index,
                        image_path: None,
                    });
                    *chunk_index += 1;
                    current_chunk.clear();
                }
                // Force split the long sentence at MAX_CHUNK_SIZE boundary
                let mut remaining = sentence;
                while remaining.len() > MAX_CHUNK_SIZE {
                    // Try to split at a character boundary
                    let split_at = find_split_point(remaining, MAX_CHUNK_SIZE);
                    chunks.push(DocChunk {
                        file_path: file_path.to_string(),
                        chunk_type: "text".to_string(),
                        content: remaining[..split_at].to_string(),
                        heading: heading.to_string(),
                        chunk_index: *chunk_index,
                        image_path: None,
                    });
                    *chunk_index += 1;
                    remaining = &remaining[split_at..];
                }
                if !remaining.is_empty() {
                    current_chunk = remaining.to_string();
                }
            } else {
                if !current_chunk.is_empty() {
                    current_chunk.push(' ');
                }
                current_chunk.push_str(sentence);
            }
        }
        
        // Flush remaining
        if current_chunk.trim().len() >= MIN_CHUNK_SIZE {
            chunks.push(DocChunk {
                file_path: file_path.to_string(),
                chunk_type: "text".to_string(),
                content: current_chunk.trim().to_string(),
                heading: heading.to_string(),
                chunk_index: *chunk_index,
                image_path: None,
            });
            *chunk_index += 1;
        }
    }
}

/// Split text into sentences using Chinese and English sentence boundaries.
fn split_by_sentences(text: &str) -> Vec<String> {
    let mut sentences = Vec::new();
    let mut current = String::new();
    
    for ch in text.chars() {
        current.push(ch);
        // Chinese sentence endings: 。！？；
        // English sentence endings: . ! ? ;
        if ch == '。' || ch == '！' || ch == '？' || ch == '；' 
            || ch == '.' || ch == '!' || ch == '?' || ch == ';' {
            let trimmed = current.trim();
            if !trimmed.is_empty() {
                sentences.push(trimmed.to_string());
            }
            current.clear();
        }
    }
    
    // Don't forget remaining text
    let trimmed = current.trim();
    if !trimmed.is_empty() {
        sentences.push(trimmed.to_string());
    }
    
    sentences
}

/// Find a safe split point within byte limit, preferring character boundaries.
fn find_split_point(text: &str, max_bytes: usize) -> usize {
    if text.len() <= max_bytes {
        return text.len();
    }
    
    // Find the last character boundary within max_bytes
    let mut split_at = max_bytes;
    while split_at > 0 && !text.is_char_boundary(split_at) {
        split_at -= 1;
    }
    
    // Try to split at a space or punctuation for better readability
    let search_start = if split_at > 100 { split_at - 100 } else { 0 };
    for i in (search_start..=split_at).rev() {
        if text.is_char_boundary(i) {
            let ch = text[i..].chars().next();
            if let Some(c) = ch {
                if c == ' ' || c == '\n' || c == '。' || c == '，' || c == '.' || c == ',' {
                    return i + c.len_utf8();
                }
            }
        }
    }
    
    split_at
}

/// Flush table lines into a single table chunk.
fn flush_table_chunk(
    chunks: &mut Vec<DocChunk>,
    file_path: &str,
    heading: &str,
    table_lines: &[String],
    chunk_index: &mut i32,
) {
    if table_lines.is_empty() {
        return;
    }
    
    let table_content = table_lines.join("\n");
    
    chunks.push(DocChunk {
        file_path: file_path.to_string(),
        chunk_type: "table".to_string(),
        content: table_content,
        heading: heading.to_string(),
        chunk_index: *chunk_index,
        image_path: None,
    });
    *chunk_index += 1;
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_chunk_simple_document() {
        let markdown = r#"# Title

## Introduction

This is the introduction paragraph with enough content to meet the minimum chunk size requirement for indexing purposes.

## Methods

Here we describe the methods used in this study.

### Data Collection

Data was collected from multiple sources.

| Column A | Column B |
|----------|----------|
| Value 1  | Value 2  |

## Conclusion

This is the conclusion of the document with sufficient content to be indexed as a meaningful chunk for search purposes.

![Figure 1](images/fig1.png)
"#;
        
        let chunks = chunk_markdown(markdown, "test.pdf");
        
        // Should have: intro text, methods text, data collection text, table, conclusion text, image
        assert!(chunks.len() >= 4, "Expected at least 4 chunks, got {}", chunks.len());
        
        // Check chunk types
        let types: Vec<&str> = chunks.iter().map(|c| c.chunk_type.as_str()).collect();
        assert!(types.contains(&"text"));
        assert!(types.contains(&"table"));
        assert!(types.contains(&"image"));
    }
    
    #[test]
    fn test_chunk_table_only() {
        let markdown = r#"| Name | Value |
|------|-------|
| A    | 1     |
| B    | 2     |
"#;
        
        let chunks = chunk_markdown(markdown, "table.pdf");
        
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].chunk_type, "table");
        assert!(chunks[0].content.contains("| Name |"));
    }
    
    #[test]
    fn test_parse_image() {
        let (alt, path) = parse_image("![My Figure](images/figure.png)").unwrap();
        assert_eq!(alt, "My Figure");
        assert_eq!(path, "images/figure.png");
    }
    
    #[test]
    fn test_chunk_preserves_heading_context() {
        let markdown = r#"## Section A

Content of section A with enough text to be considered a valid chunk for indexing.

## Section B

Content of section B with enough text to be considered a valid chunk for indexing.
"#;
        
        let chunks = chunk_markdown(markdown, "doc.pdf");
        
        // Find chunks with headings
        let section_a = chunks.iter().find(|c| c.heading == "Section A");
        let section_b = chunks.iter().find(|c| c.heading == "Section B");
        
        assert!(section_a.is_some(), "Should have chunk with Section A heading");
        assert!(section_b.is_some(), "Should have chunk with Section B heading");
    }
    
    #[test]
    fn test_chunk_preserves_short_poem() {
        // A short poem should be kept as one chunk
        let markdown = r#"## 静夜思

床前明月光，疑是地上霜。
举头望明月，低头思故乡。
"#;
        
        let chunks = chunk_markdown(markdown, "poem.pdf");
        
        // Should be one chunk containing the whole poem
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].content.contains("床前明月光"));
        assert!(chunks[0].content.contains("低头思故乡"));
        assert_eq!(chunks[0].heading, "静夜思");
    }
    
    #[test]
    fn test_chunk_chinese_document() {
        // Chinese text with headings
        let markdown = r#"## 第一单元

日月经天，江河行地，春风夏雨，秋霜冬雪，大自然生生不息，四时景物美不胜收。

### 春

盼望着，盼望着，东风来了，春天的脚步近了。
小草偷偷地从土里钻出来，嫩嫩的，绿绿的。
"#;
        
        let chunks = chunk_markdown(markdown, "chinese.pdf");
        
        // Should have chunks for each section
        assert!(chunks.len() >= 2, "Expected at least 2 chunks, got {}", chunks.len());
        
        // Check heading context is preserved
        let unit_chunk = chunks.iter().find(|c| c.heading == "第一单元");
        assert!(unit_chunk.is_some(), "Should have chunk with 第一单元 heading");
    }
    
    #[test]
    fn test_split_long_paragraph_by_sentence() {
        // Create a very long paragraph that exceeds MAX_CHUNK_SIZE
        let long_sentence = "这是一句很长的话。".repeat(600); // ~7200 bytes
        let markdown = format!("## Long Section\n\n{}\n", long_sentence);
        
        let chunks = chunk_markdown(&markdown, "long.pdf");
        
        // Should be split into multiple chunks
        assert!(chunks.len() >= 2, "Long paragraph should be split, got {} chunks", chunks.len());
        
        // Each chunk should be within MAX_CHUNK_SIZE
        for chunk in &chunks {
            assert!(chunk.content.len() <= MAX_CHUNK_SIZE, 
                "Chunk exceeds MAX_CHUNK_SIZE: {} bytes", chunk.content.len());
        }
    }
    
    #[test]
    fn test_h1_heading_detected() {
        let markdown = r#"# Main Title

Some content here with enough text to be a valid chunk for indexing purposes.
"#;
        
        let chunks = chunk_markdown(markdown, "h1.pdf");
        
        let main_title = chunks.iter().find(|c| c.heading == "Main Title");
        assert!(main_title.is_some(), "Should detect H1 heading");
    }
}
