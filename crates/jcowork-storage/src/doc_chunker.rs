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
const MAX_CHUNK_SIZE: usize = 1000;

/// Minimum characters for a meaningful chunk
const MIN_CHUNK_SIZE: usize = 50;

/// Chunk a Markdown document into semantic units.
///
/// The chunker splits by:
/// 1. Headings (## or ###) for text sections
/// 2. Tables (each table becomes its own chunk)
/// 3. Images (each image becomes its own chunk)
///
/// Long text sections are further split by paragraphs if they exceed MAX_CHUNK_SIZE.
pub fn chunk_markdown(markdown: &str, file_path: &str) -> Vec<DocChunk> {
    let mut chunks = Vec::new();
    let mut chunk_index = 0;
    
    let mut current_heading = String::new();
    let mut current_text = String::new();
    let mut in_table = false;
    let mut table_lines: Vec<String> = Vec::new();
    
    for line in markdown.lines() {
        let trimmed = line.trim();
        
        // Check for heading (## or ###)
        if trimmed.starts_with("## ") || trimmed.starts_with("### ") {
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
/// If text exceeds MAX_CHUNK_SIZE, split by paragraphs.
fn flush_text_chunks(
    chunks: &mut Vec<DocChunk>,
    file_path: &str,
    heading: &str,
    text: &str,
    chunk_index: &mut i32,
) {
    let text = text.trim();
    if text.len() < MIN_CHUNK_SIZE {
        // Too small, skip
        return;
    }
    
    if text.len() <= MAX_CHUNK_SIZE {
        chunks.push(DocChunk {
            file_path: file_path.to_string(),
            chunk_type: "text".to_string(),
            content: text.to_string(),
            heading: heading.to_string(),
            chunk_index: *chunk_index,
            image_path: None,
        });
        *chunk_index += 1;
    } else {
        // Split by paragraphs (double newline)
        let paragraphs: Vec<&str> = text.split("\n\n").collect();
        let mut current_chunk = String::new();
        
        for para in paragraphs {
            if current_chunk.len() + para.len() > MAX_CHUNK_SIZE && !current_chunk.is_empty() {
                // Flush current chunk
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
            
            current_chunk.push_str(para);
            current_chunk.push_str("\n\n");
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
}
