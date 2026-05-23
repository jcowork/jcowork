//! Sandboxed file operations within a user's workspace.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use tokio::fs;
use tracing::info;

/// Manages file operations within a user's sandboxed workspace directory.
///
/// All file operations are restricted to the user's workspace directory.
/// Paths that attempt to escape the workspace (via `..` or absolute paths) are rejected.
#[derive(Debug, Clone)]
pub struct FileStore {
    workspace_root: PathBuf,
}

impl FileStore {
    /// Create a new FileStore rooted at `workspace_root`.
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            workspace_root: workspace_root.into(),
        }
    }

    /// Validate that a path is within the workspace root.
    fn validate_path(&self, path: &str) -> Result<PathBuf> {
        let resolved = self.workspace_root.join(path);
        let canonical = if self.workspace_root.exists() {
            let canonical_root = self.workspace_root.canonicalize()?;
            let parent = resolved.parent().context("Invalid path")?;
            if parent.exists() {
                let canonical_resolved = resolved.canonicalize().unwrap_or_else(|_| resolved.clone());
                if !canonical_resolved.starts_with(&canonical_root) {
                    anyhow::bail!("Path escapes workspace: {}", path);
                }
            }
            resolved
        } else {
            resolved
        };

        // Also check for .. components
        let mut components = std::path::Path::new(path).components();
        if components.any(|c| matches!(c, std::path::Component::ParentDir)) {
            anyhow::bail!("Path contains parent directory reference: {}", path);
        }

        Ok(canonical)
    }

    /// Read a file's contents.
    pub async fn read_file(&self, path: &str) -> Result<String> {
        let full_path = self.validate_path(path)?;
        let content = fs::read_to_string(&full_path).await?;
        info!(path = path, "Read file");
        Ok(content)
    }

    /// Write content to a file, creating parent directories as needed.
    pub async fn write_file(&self, path: &str, content: &str) -> Result<()> {
        let full_path = self.validate_path(path)?;
        if let Some(parent) = full_path.parent() {
            fs::create_dir_all(parent).await?;
        }
        fs::write(&full_path, content).await?;
        info!(path = path, "Wrote file");
        Ok(())
    }

    /// List files in a directory (non-recursive).
    pub async fn list_dir(&self, path: &str) -> Result<Vec<String>> {
        let full_path = self.validate_path(path)?;
        let mut entries = Vec::new();
        let mut read_dir = fs::read_dir(&full_path).await?;

        while let Some(entry) = read_dir.next_entry().await? {
            if let Some(name) = entry.file_name().to_str() {
                entries.push(name.to_string());
            }
        }

        entries.sort();
        Ok(entries)
    }

    /// Delete a file.
    pub async fn delete_file(&self, path: &str) -> Result<()> {
        let full_path = self.validate_path(path)?;
        fs::remove_file(&full_path).await?;
        info!(path = path, "Deleted file");
        Ok(())
    }

    /// Check if a file exists.
    pub async fn exists(&self, path: &str) -> Result<bool> {
        let full_path = self.validate_path(path)?;
        Ok(full_path.exists())
    }

    /// Get the workspace root path.
    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    /// Initialize the workspace directory.
    pub async fn init(&self) -> Result<()> {
        fs::create_dir_all(&self.workspace_root).await?;
        info!(root = ?self.workspace_root, "Initialized workspace");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_file_store_basic() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileStore::new(dir.path());

        store.init().await.unwrap();

        // Write
        store.write_file("test.txt", "hello world").await.unwrap();

        // Read
        let content = store.read_file("test.txt").await.unwrap();
        assert_eq!(content, "hello world");

        // Exists
        assert!(store.exists("test.txt").await.unwrap());
        assert!(!store.exists("nonexistent.txt").await.unwrap());

        // Delete
        store.delete_file("test.txt").await.unwrap();
        assert!(!store.exists("test.txt").await.unwrap());
    }

    #[tokio::test]
    async fn test_path_traversal_blocked() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileStore::new(dir.path());

        // Path traversal should be blocked
        assert!(store.validate_path("../../../etc/passwd").is_err());
        assert!(store.validate_path("../../secret").is_err());
    }
}
