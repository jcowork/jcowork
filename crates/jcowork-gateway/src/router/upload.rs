//! File upload endpoints: PDF upload/parse and general document upload.

use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use axum::extract::Multipart;
use serde::Deserialize;

use super::{AppState, AuthUser};
use jcowork_storage::{FileStore, WorkspaceIndex};

/// Inline Python script for PDF text extraction using pdftext.
/// Must match the script in jcowork-tools/pdf_parse.rs.
const PDF_PARSE_SCRIPT: &str = r#"
import sys
import os

path = sys.argv[1]

if not os.path.isfile(path):
    print(f"Error: path does not exist: {path}", file=sys.stderr)
    sys.exit(1)

from pdftext.extraction import plain_text_output

try:
    text = plain_text_output(path)
    print(text)
except Exception as e:
    print(f"Error parsing PDF: {e}", file=sys.stderr)
    sys.exit(1)
"#;

/// Resolve the python binary inside the user's ~/.jcowork venv.
fn venv_python_bin() -> String {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| "/tmp".to_string());
    if cfg!(windows) {
        format!("{}\\.jcowork\\venv\\Scripts\\python.exe", home)
    } else {
        format!("{}/.jcowork/venv/bin/python", home)
    }
}

/// Run the pdftext extraction script on the given file and return the text.
async fn parse_pdf_with_venv(file_path: &std::path::Path) -> String {
    let python_bin = venv_python_bin();
    if !std::path::Path::new(&python_bin).exists() {
        return "[Python venv not found. Run scripts/setup-python.sh first.]".to_string();
    }
    match tokio::time::timeout(
        std::time::Duration::from_secs(120),
        tokio::process::Command::new(&python_bin)
            .arg("-c")
            .arg(PDF_PARSE_SCRIPT)
            .arg(file_path)
            .output(),
    ).await {
        Ok(Ok(output)) if output.status.success() => {
            let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
            // Truncate if too large
            if text.len() > 200 * 1024 {
                text.truncate(200 * 1024);
                text.push_str("\n\n[... OUTPUT TRUNCATED: document exceeds 200KB ...]");
            }
            text
        }
        Ok(Ok(output)) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            format!("[PDF parse error: {}]", stderr)
        }
        Ok(Err(e)) => format!("[Failed to run PDF parser: {}]", e),
        Err(_) => "[PDF parsing timed out after 120s]".to_string(),
    }
}

pub(crate) async fn upload_pdf(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    mut multipart: Multipart,
) -> impl IntoResponse {
    let workspace_root = format!("{}/{}/workspace", state.data_dir, auth_user.user_id);
    let _ = tokio::fs::create_dir_all(&workspace_root).await;
    let uploads_dir = format!("{}/uploads", workspace_root);
    let _ = tokio::fs::create_dir_all(&uploads_dir).await;

    let mut result_files: Vec<serde_json::Value> = Vec::new();

    while let Ok(Some(field)) = multipart.next_field().await {
        let filename = field
            .file_name()
            .unwrap_or("uploaded.pdf")
            .to_string();
        // Sanitize filename
        let safe_name = filename
            .replace("..", "")
            .replace('/', "_")
            .replace('\\', "_");
        let file_path = format!("{}/{}", uploads_dir, safe_name);
        let relative_path = format!("uploads/{}", safe_name);

        let data = match field.bytes().await {
            Ok(d) => d,
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({ "error": format!("Failed to read file: {}", e) })),
                ).into_response();
            }
        };

        // Save PDF to workspace
        if let Err(e) = tokio::fs::write(&file_path, &data).await {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("Failed to save file: {}", e) })),
            ).into_response();
        }

        let parsed_text = parse_pdf_with_venv(std::path::Path::new(&file_path)).await;

        result_files.push(serde_json::json!({
            "filename": safe_name,
            "path": relative_path,
            "size": data.len(),
            "text": parsed_text,
        }));
    }

    if result_files.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "No files uploaded" })),
        ).into_response();
    }

    (StatusCode::OK, Json(serde_json::json!({ "files": result_files }))).into_response()
}

// --- Parse workspace PDF API ---

#[derive(Debug, Deserialize)]
pub(crate) struct ParsePdfRequest {
    path: String,
}

/// Parse a PDF file that already exists in the user's workspace.
/// Returns the extracted text content.
pub(crate) async fn parse_workspace_pdf(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Json(body): Json<ParsePdfRequest>,
) -> impl IntoResponse {
    let workspace_root = format!("{}/{}/workspace", state.data_dir, auth_user.user_id);
    let store = FileStore::new(&workspace_root);

    // Validate path is within workspace
    let full_path = match store.validate_path_public(&body.path) {
        Ok(p) => p,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": e.to_string() })),
            ).into_response();
        }
    };

    // Check file exists
    if !full_path.is_file() {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": format!("File not found: {}", body.path) })),
        ).into_response();
    }

    let parsed_text = parse_pdf_with_venv(&full_path).await;

    let filename = body.path.rsplit('/').next().unwrap_or(&body.path);
    (StatusCode::OK, Json(serde_json::json!({
        "filename": filename,
        "path": body.path,
        "text": parsed_text,
    }))).into_response()
}

// --- General File Upload API ---

/// Allowed file extensions for upload.
const ALLOWED_UPLOAD_EXTENSIONS: &[&str] = &["pdf", "md", "html", "htm", "xlsx", "xls", "docx", "doc"];

pub(crate) async fn upload_file(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    mut multipart: Multipart,
) -> impl IntoResponse {
    let workspace_root = format!("{}/{}/workspace", state.data_dir, auth_user.user_id);
    let _ = tokio::fs::create_dir_all(&workspace_root).await;

    // Optional target directory (relative to workspace root)
    let mut target_dir: Option<String> = None;
    let mut uploaded_files: Vec<serde_json::Value> = Vec::new();

    // First pass: collect all fields
    let mut fields_data: Vec<(String, String, Vec<u8>)> = Vec::new();

    while let Ok(Some(field)) = multipart.next_field().await {
        let field_name = field.name().unwrap_or("").to_string();
        let filename = field.file_name().unwrap_or("").to_string();
        let data = match field.bytes().await {
            Ok(d) => d.to_vec(),
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({ "error": format!("Failed to read file: {}", e) })),
                ).into_response();
            }
        };
        fields_data.push((field_name, filename, data));
    }

    // Extract target_dir from form fields
    for (field_name, filename, data) in &fields_data {
        if field_name == "path" && filename.is_empty() {
            // This is the target directory field; value is in data bytes
            target_dir = Some(String::from_utf8_lossy(data).trim().to_string());
        }
    }

    // Process file fields
    for (field_name, filename, data) in &fields_data {
        if field_name == "path" {
            // Skip the directory path field
            continue;
        }

        if filename.is_empty() {
            continue;
        }

        // Check file extension
        let ext = filename.rsplit('.').next().unwrap_or("").to_lowercase();
        if !ALLOWED_UPLOAD_EXTENSIONS.contains(&ext.as_str()) {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": format!("File type '{}' not allowed. Allowed types: pdf, md, html, xlsx, xls, docx, doc", ext)
                })),
            ).into_response();
        }

        // Sanitize filename
        let safe_name = filename
            .replace("..", "")
            .replace('/', "_")
            .replace('\\', "_");

        // Determine target directory
        let dest_dir = if let Some(ref dir) = target_dir {
            if dir.is_empty() || dir == "." {
                workspace_root.clone()
            } else {
                format!("{}/{}", workspace_root, dir)
            }
        } else {
            workspace_root.clone()
        };

        // Ensure target directory exists
        if let Err(e) = tokio::fs::create_dir_all(&dest_dir).await {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("Failed to create directory: {}", e) })),
            ).into_response();
        }

        let file_path = format!("{}/{}", dest_dir, safe_name);
        let relative_path = if let Some(ref dir) = target_dir {
            if dir.is_empty() || dir == "." {
                safe_name.clone()
            } else {
                format!("{}/{}", dir, safe_name)
            }
        } else {
            safe_name.clone()
        };

        // Save file
        if let Err(e) = tokio::fs::write(&file_path, data).await {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("Failed to save file: {}", e) })),
            ).into_response();
        }

        // Index the uploaded file
        let mut index_error: Option<String> = None;
        match WorkspaceIndex::cached(&state.data_dir, &auth_user.user_id).await {
            Ok(index) => {
                if let Err(e) = index.add_document(&relative_path, &workspace_root).await {
                    let err_msg = format!("Failed to index {}: {}", relative_path, e);
                    tracing::error!(file = %relative_path, err = %e, "Failed to index uploaded file");
                    index_error = Some(err_msg);
                }
            }
            Err(e) => {
                let err_msg = format!("Failed to open index database: {}", e);
                tracing::error!(user_id = %auth_user.user_id, err = %e, "Failed to initialize workspace index");
                index_error = Some(err_msg);
            }
        }

        uploaded_files.push(serde_json::json!({
            "filename": safe_name,
            "path": relative_path,
            "size": data.len(),
            "indexed": index_error.is_none(),
            "index_error": index_error,
        }));
    }

    if uploaded_files.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "No files uploaded" })),
        ).into_response();
    }

    (StatusCode::OK, Json(serde_json::json!({ "files": uploaded_files }))).into_response()
}
