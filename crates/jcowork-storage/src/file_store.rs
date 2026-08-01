//! Sandboxed file operations within a user's workspace.

use anyhow::Result;
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

    /// Validate that a path is within the workspace root (public wrapper).
    pub fn validate_path_public(&self, path: &str) -> Result<PathBuf> {
        self.validate_path(path)
    }

    /// Validate that a path is within the workspace root.
    fn validate_path(&self, path: &str) -> Result<PathBuf> {
        let resolved = self.workspace_root.join(path);

        // Check for .. components first (prevents traversal attacks)
        let mut components = std::path::Path::new(path).components();
        if components.any(|c| matches!(c, std::path::Component::ParentDir)) {
            anyhow::bail!("Path contains parent directory reference: {}", path);
        }

        // Canonicalize to check for path escapes
        if self.workspace_root.exists() {
            let canonical_root = self.workspace_root.canonicalize()?;
            // For non-existent paths, canonicalize the parent and rejoin the filename.
            // This handles macOS /private prefix and other symlink differences.
            let canonical_resolved = if resolved.exists() {
                resolved.canonicalize()?
            } else if let Some(parent) = resolved.parent() {
                if parent.exists() {
                    let canonical_parent = parent.canonicalize()?;
                    match resolved.file_name() {
                        Some(name) => canonical_parent.join(name),
                        None => canonical_parent,
                    }
                } else {
                    // Parent doesn't exist — walk up to find an existing ancestor,
                    // canonicalize it, then rejoin the remaining components.
                    Self::canonicalize_nonexistent(&resolved, &canonical_root)?
                }
            } else {
                resolved.clone()
            };

            if !canonical_resolved.starts_with(&canonical_root) {
                anyhow::bail!("Path escapes workspace: {}", path);
            }
        }

        Ok(resolved)
    }

    /// Canonicalize a path whose ancestors may not all exist yet.
    fn canonicalize_nonexistent(path: &Path, canonical_root: &Path) -> Result<PathBuf> {
        // Walk up until we find an existing ancestor, canonicalize it,
        // then rejoin the remaining non-existent components.
        let mut existing_ancestor = path.to_path_buf();
        let mut remaining: Vec<String> = Vec::new();
        while !existing_ancestor.exists() {
            if let Some(name) = existing_ancestor.file_name() {
                remaining.push(name.to_string_lossy().to_string());
            }
            match existing_ancestor.parent() {
                Some(p) => existing_ancestor = p.to_path_buf(),
                None => break,
            }
        }
        let canonical_base = existing_ancestor.canonicalize()?;
        let mut result = canonical_base;
        for name in remaining.into_iter().rev() {
            result = result.join(name);
        }
        // If we walked past the root, the path escapes
        if !result.starts_with(canonical_root) {
            anyhow::bail!("Path escapes workspace: {}", path.display());
        }
        Ok(result)
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

    /// Create a directory (and all parent directories).
    pub async fn create_dir(&self, path: &str) -> Result<()> {
        let full_path = self.validate_path(path)?;
        fs::create_dir_all(&full_path).await?;
        info!(path = path, "Created directory");
        Ok(())
    }

    /// Remove a directory and all its contents.
    pub async fn remove_dir(&self, path: &str) -> Result<()> {
        let full_path = self.validate_path(path)?;
        fs::remove_dir_all(&full_path).await?;
        info!(path = path, "Removed directory");
        Ok(())
    }

    /// Move or rename a file/directory.
    pub async fn move_path(&self, from: &str, to: &str) -> Result<()> {
        let from_path = self.validate_path(from)?;
        let to_path = self.validate_path(to)?;
        if let Some(parent) = to_path.parent() {
            fs::create_dir_all(parent).await?;
        }
        fs::rename(&from_path, &to_path).await?;
        info!(from = from, to = to, "Moved path");
        Ok(())
    }

    /// Copy a file to a new location.
    pub async fn copy_file(&self, from: &str, to: &str) -> Result<()> {
        let from_path = self.validate_path(from)?;
        let to_path = self.validate_path(to)?;
        if let Some(parent) = to_path.parent() {
            fs::create_dir_all(parent).await?;
        }
        fs::copy(&from_path, &to_path).await?;
        info!(from = from, to = to, "Copied file");
        Ok(())
    }

    /// List directory entries with type information (non-recursive).
    ///
    /// Returns a list of "name\ttype" where type is "file" or "dir".
    pub async fn list_dir_detailed(&self, path: &str) -> Result<Vec<String>> {
        let full_path = self.validate_path(path)?;
        let mut entries = Vec::new();
        let mut read_dir = fs::read_dir(&full_path).await?;

        while let Some(entry) = read_dir.next_entry().await? {
            if let Some(name) = entry.file_name().to_str() {
                let file_type = entry.file_type().await?;
                let type_str = if file_type.is_dir() { "dir" } else { "file" };
                entries.push(format!("{}\t{}", name, type_str));
            }
        }

        entries.sort();
        Ok(entries)
    }

    /// Recursively list all files under a directory.
    ///
    /// Returns relative paths from the given directory.
    pub async fn list_dir_recursive(&self, path: &str) -> Result<Vec<String>> {
        let full_path = self.validate_path(path)?;
        let base = full_path.clone();
        let mut results = Vec::new();
        Self::collect_entries_recursive(&full_path, &base, &mut results).await?;
        results.sort();
        Ok(results)
    }

    async fn collect_entries_recursive(
        current: &Path,
        base: &Path,
        results: &mut Vec<String>,
    ) -> Result<()> {
        let mut read_dir = fs::read_dir(current).await?;
        while let Some(entry) = read_dir.next_entry().await? {
            let path = entry.path();
            let file_type = entry.file_type().await?;
            if file_type.is_dir() {
                // Skip hidden directories like .git, node_modules, target
                if let Some(name) = entry.file_name().to_str() {
                    if name.starts_with('.') || name == "node_modules" || name == "target" || name == "__pycache__" {
                        continue;
                    }
                }
                Box::pin(Self::collect_entries_recursive(&path, base, results)).await?;
            } else if file_type.is_file() {
                if let Ok(rel) = path.strip_prefix(base) {
                    results.push(rel.to_string_lossy().to_string());
                }
            }
        }
        Ok(())
    }

    /// Search file contents matching a pattern (substring match).
    ///
    /// Returns a list of "path:line_number:matched_line".
    pub async fn search_content(&self, pattern: &str, path: &str) -> Result<Vec<String>> {
        let full_path = self.validate_path(path)?;
        let base = full_path.clone();
        let mut results = Vec::new();
        Self::search_recursive(&full_path, &base, pattern, &mut results).await?;
        Ok(results)
    }

    async fn search_recursive(
        current: &Path,
        base: &Path,
        pattern: &str,
        results: &mut Vec<String>,
    ) -> Result<()> {
        let mut read_dir = fs::read_dir(current).await?;
        while let Some(entry) = read_dir.next_entry().await? {
            let path = entry.path();
            let file_type = entry.file_type().await?;
            if file_type.is_dir() {
                if let Some(name) = entry.file_name().to_str() {
                    if name.starts_with('.') || name == "node_modules" || name == "target" || name == "__pycache__" {
                        continue;
                    }
                }
                Box::pin(Self::search_recursive(&path, base, pattern, results)).await?;
            } else if file_type.is_file() {
                Self::search_file(&path, base, pattern, results).await?;
            }
        }
        Ok(())
    }

    async fn search_file(
        path: &Path,
        base: &Path,
        pattern: &str,
        results: &mut Vec<String>,
    ) -> Result<()> {
        // Skip binary/large files
        if let Some(ext) = path.extension() {
            let ext = ext.to_string_lossy().to_lowercase();
            if matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "bmp" | "ico" | "pdf" | "zip" | "gz" | "tar" | "exe" | "dll" | "so" | "dylib" | "class" | "jar" | "wasm") {
                return Ok(());
            }
        }
        let metadata = fs::metadata(path).await?;
        if metadata.len() > 5 * 1024 * 1024 {
            return Ok(()); // skip files > 5MB
        }
        let content = match fs::read_to_string(path).await {
            Ok(c) => c,
            Err(_) => return Ok(()), // skip non-UTF8 files
        };
        let rel = path.strip_prefix(base).unwrap_or(path).to_string_lossy().to_string();
        for (i, line) in content.lines().enumerate() {
            if line.contains(pattern) {
                let preview = if line.len() > 200 {
                    // Avoid splitting a multi-byte UTF-8 character (e.g. Chinese text)
                    let mut end = 200;
                    while end > 0 && !line.is_char_boundary(end) {
                        end -= 1;
                    }
                    &line[..end]
                } else {
                    line
                };
                results.push(format!("{}:{}:{}", rel, i + 1, preview));
            }
        }
        Ok(())
    }

    /// Get file metadata (size, type, modified time).
    pub async fn file_info(&self, path: &str) -> Result<String> {
        let full_path = self.validate_path(path)?;
        let metadata = fs::metadata(&full_path).await?;
        let file_type = if metadata.is_dir() { "directory" } else { "file" };
        let size = metadata.len();
        let modified = metadata.modified()?;
        let modified_str = modified
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .map(|d| {
                let dt = chrono::DateTime::from_timestamp(d.as_secs() as i64, 0)
                    .unwrap_or_default();
                dt.format("%Y-%m-%d %H:%M:%S").to_string()
            })
            .unwrap_or_else(|_| "unknown".to_string());
        Ok(format!("path: {}\ntype: {}\nsize: {} bytes\nmodified: {}", path, file_type, size, modified_str))
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

    #[tokio::test]
    async fn test_create_and_remove_dir() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileStore::new(dir.path());
        store.init().await.unwrap();

        // Create nested directories
        store.create_dir("project/src/components").await.unwrap();
        assert!(store.exists("project/src/components").await.unwrap());

        // Write a file in the nested dir
        store.write_file("project/src/components/Button.tsx", "export const Button = () => null;").await.unwrap();
        assert!(store.exists("project/src/components/Button.tsx").await.unwrap());

        // Remove the directory and its contents
        store.remove_dir("project").await.unwrap();
        assert!(!store.exists("project").await.unwrap());
    }

    #[tokio::test]
    async fn test_move_and_copy() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileStore::new(dir.path());
        store.init().await.unwrap();

        // Write original file
        store.write_file("original.txt", "hello").await.unwrap();

        // Copy
        store.copy_file("original.txt", "copy.txt").await.unwrap();
        assert_eq!(store.read_file("copy.txt").await.unwrap(), "hello");
        assert!(store.exists("original.txt").await.unwrap()); // original still there

        // Move
        store.move_path("original.txt", "moved.txt").await.unwrap();
        assert!(!store.exists("original.txt").await.unwrap()); // original gone
        assert_eq!(store.read_file("moved.txt").await.unwrap(), "hello");
    }

    #[tokio::test]
    async fn test_list_dir_detailed() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileStore::new(dir.path());
        store.init().await.unwrap();

        store.create_dir("subdir").await.unwrap();
        store.write_file("file1.txt", "a").await.unwrap();
        store.write_file("file2.txt", "b").await.unwrap();

        let entries = store.list_dir_detailed(".").await.unwrap();
        // Should contain file1.txt\tfile, file2.txt\tfile, subdir\tdir
        let joined = entries.join("\n");
        assert!(joined.contains("file1.txt\tfile"));
        assert!(joined.contains("file2.txt\tfile"));
        assert!(joined.contains("subdir\tdir"));
    }

    #[tokio::test]
    async fn test_list_dir_recursive() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileStore::new(dir.path());
        store.init().await.unwrap();

        store.write_file("project/index.html", "<html>").await.unwrap();
        store.write_file("project/css/style.css", "body {}").await.unwrap();
        store.write_file("project/js/app.js", "console.log(1)").await.unwrap();
        store.write_file("project/.git/config", "[core]").await.unwrap(); // should be skipped

        let files = store.list_dir_recursive("project").await.unwrap();
        assert!(files.contains(&"index.html".to_string()));
        assert!(files.contains(&"css/style.css".to_string()));
        assert!(files.contains(&"js/app.js".to_string()));
        // .git should be skipped
        assert!(!files.iter().any(|f| f.contains(".git")));
    }

    #[tokio::test]
    async fn test_search_content() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileStore::new(dir.path());
        store.init().await.unwrap();

        store.write_file("app.py", "def hello():\n    print('hello world')\n    return True").await.unwrap();
        store.write_file("utils.py", "# utility functions\nprint('nothing here')").await.unwrap();

        let results = store.search_content("hello", ".").await.unwrap();
        assert!(!results.is_empty());
        // Should find 'hello' in app.py
        let found = results.iter().find(|r| r.contains("app.py"));
        assert!(found.is_some());
        assert!(found.unwrap().contains("hello"));

        // Search for something that doesn't exist
        let no_results = store.search_content("nonexistent_pattern_xyz", ".").await.unwrap();
        assert!(no_results.is_empty());
    }

    #[tokio::test]
    async fn test_file_info() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileStore::new(dir.path());
        store.init().await.unwrap();

        store.write_file("test.txt", "hello world").await.unwrap();
        store.create_dir("mydir").await.unwrap();

        let info = store.file_info("test.txt").await.unwrap();
        assert!(info.contains("type: file"));
        assert!(info.contains("size: 11 bytes"));

        let dir_info = store.file_info("mydir").await.unwrap();
        assert!(dir_info.contains("type: directory"));
    }

    #[tokio::test]
    async fn test_html_page_iteration_workflow() {
        // Simulate the full workflow: create an HTML page, modify it iteratively,
        // and verify each save/reload cycle works correctly.
        let dir = tempfile::tempdir().unwrap();
        let store = FileStore::new(dir.path());
        store.init().await.unwrap();

        // Step 1: Create project structure
        store.create_dir("myapp/css").await.unwrap();
        store.create_dir("myapp/js").await.unwrap();

        // Step 2: Write initial HTML page (v1 - minimal)
        let html_v1 = r##"<!DOCTYPE html>
<html>
<head><title>My Page</title></head>
<body>
<h1>Hello</h1>
</body>
</html>"##;
        store.write_file("myapp/index.html", html_v1).await.unwrap();

        // Verify it was saved correctly
        let read_v1 = store.read_file("myapp/index.html").await.unwrap();
        assert_eq!(read_v1, html_v1);
        assert!(read_v1.contains("<h1>Hello</h1>"));

        // Step 3: User wants a styled page - modify (v2 - add CSS and content)
        let html_v2 = r##"<!DOCTYPE html>
<html>
<head>
<title>My Page</title>
<style>
body { font-family: sans-serif; background: #f0f0f0; }
h1 { color: #333; }
</style>
</head>
<body>
<h1>Hello World</h1>
<p>Welcome to my page!</p>
</body>
</html>"##;
        store.write_file("myapp/index.html", html_v2).await.unwrap();

        // Verify v2 saved correctly (overwrites v1)
        let read_v2 = store.read_file("myapp/index.html").await.unwrap();
        assert_eq!(read_v2, html_v2);
        assert!(read_v2.contains("<style>"));
        assert!(read_v2.contains("Hello World"));
        assert!(!read_v2.contains("<h1>Hello</h1>")); // v1 content gone

        // Step 4: User wants interactivity - modify again (v3 - add JS)
        let html_v3 = r##"<!DOCTYPE html>
<html>
<head>
<title>My Page</title>
<style>
body { font-family: sans-serif; background: #f0f0f0; margin: 40px; }
h1 { color: #333; }
button { padding: 10px 20px; cursor: pointer; }
</style>
</head>
<body>
<h1>Hello World</h1>
<p>Welcome to my page!</p>
<button id="btn">Click me</button>
<p id="counter">Clicks: 0</p>
<script>
let count = 0;
document.getElementById('btn').addEventListener('click', () => {
  count++;
  document.getElementById('counter').textContent = 'Clicks: ' + count;
});
</script>
</body>
</html>"##;
        store.write_file("myapp/index.html", html_v3).await.unwrap();

        // Verify v3 saved correctly
        let read_v3 = store.read_file("myapp/index.html").await.unwrap();
        assert_eq!(read_v3, html_v3);
        assert!(read_v3.contains("<script>"));
        assert!(read_v3.contains("addEventListener"));
        assert!(read_v3.contains("Click me"));

        // Step 5: Add a separate CSS file and JS file, update HTML to link them
        store.write_file("myapp/css/style.css", "body { margin: 0; padding: 20px; }").await.unwrap();
        store.write_file("myapp/js/app.js", "console.log('app loaded');").await.unwrap();

        let html_v4 = r##"<!DOCTYPE html>
<html>
<head>
<title>My Page</title>
<link rel="stylesheet" href="css/style.css">
</head>
<body>
<h1>Hello World</h1>
<p>Welcome to my page!</p>
<button id="btn">Click me</button>
<p id="counter">Clicks: 0</p>
<script src="js/app.js"></script>
</body>
</html>"##;
        store.write_file("myapp/index.html", html_v4).await.unwrap();

        // Verify final state
        let read_final = store.read_file("myapp/index.html").await.unwrap();
        assert!(read_final.contains("css/style.css"));
        assert!(read_final.contains("js/app.js"));

        // Step 6: Use search to find all references to a pattern across the project
        let search_results = store.search_content("Hello", "myapp").await.unwrap();
        assert!(!search_results.is_empty());
        assert!(search_results.iter().any(|r| r.contains("index.html")));

        // Step 7: Use recursive listing to verify full project structure
        let all_files = store.list_dir_recursive("myapp").await.unwrap();
        assert!(all_files.contains(&"index.html".to_string()));
        assert!(all_files.contains(&"css/style.css".to_string()));
        assert!(all_files.contains(&"js/app.js".to_string()));

        // Step 8: Check file info on the final HTML
        let info = store.file_info("myapp/index.html").await.unwrap();
        assert!(info.contains("type: file"));
        assert!(info.contains("size:"));

        // Step 9: Make a backup copy before final edit
        store.copy_file("myapp/index.html", "myapp/index.html.bak").await.unwrap();
        assert!(store.exists("myapp/index.html.bak").await.unwrap());

        // Step 10: Rename a file
        store.move_path("myapp/js/app.js", "myapp/js/main.js").await.unwrap();
        assert!(!store.exists("myapp/js/app.js").await.unwrap());
        assert!(store.exists("myapp/js/main.js").await.unwrap());

        // Verify the backup still has the old content
        let backup = store.read_file("myapp/index.html.bak").await.unwrap();
        assert!(backup.contains("js/app.js")); // backup references old name
    }

    #[tokio::test]
    async fn test_workspace_isolation_two_users() {
        // Simulate two separate users with isolated workspaces.
        // Each user's FileStore is rooted at their own workspace directory.
        // Verify that user A cannot access user B's files via any operation.

        let root = tempfile::tempdir().unwrap();
        let user_a_ws = root.path().join("user_a").join("workspace");
        let user_b_ws = root.path().join("user_b").join("workspace");
        tokio::fs::create_dir_all(&user_a_ws).await.unwrap();
        tokio::fs::create_dir_all(&user_b_ws).await.unwrap();

        let store_a = FileStore::new(&user_a_ws);
        let store_b = FileStore::new(&user_b_ws);

        // User B writes a secret file
        store_b.write_file("secret.txt", "USER_B_SECRET_DATA").await.unwrap();

        // User A writes their own file
        store_a.write_file("my_notes.txt", "User A's notes").await.unwrap();

        // === Test 1: Path traversal via ".." is blocked ===
        assert!(store_a.read_file("../secret.txt").await.is_err());
        assert!(store_a.read_file("../../user_b/workspace/secret.txt").await.is_err());
        assert!(store_a.read_file("../../../user_b/workspace/secret.txt").await.is_err());

        // === Test 2: Multi-level traversal also blocked ===
        assert!(store_a.list_dir("../../user_b").await.is_err());
        assert!(store_a.write_file("../escape.txt", "hacked").await.is_err());
        assert!(store_a.create_dir("../../user_b/workspace/hacked").await.is_err());
        assert!(store_a.delete_file("../user_b/workspace/secret.txt").await.is_err());
        assert!(store_a.search_content("SECRET", "..").await.is_err());
        assert!(store_a.copy_file("../user_b/workspace/secret.txt", "stolen.txt").await.is_err());
        assert!(store_a.move_path("../user_b/workspace/secret.txt", "stolen.txt").await.is_err());
        assert!(store_a.file_info("../user_b/workspace/secret.txt").await.is_err());

        // === Test 3: Absolute paths within workspace are treated as relative ===
        // FileStore::join treats absolute paths as relative to workspace_root,
        // but the .. check still blocks traversal
        assert!(store_a.read_file("/etc/passwd").await.is_err() ||
                store_a.read_file("/etc/passwd").await.unwrap().is_empty());

        // === Test 4: User A's operations only affect their own workspace ===
        store_a.write_file("test.txt", "A's file").await.unwrap();
        assert!(store_a.exists("test.txt").await.unwrap());
        assert!(!store_b.exists("test.txt").await.unwrap());

        store_a.create_dir("project").await.unwrap();
        assert!(store_a.exists("project").await.unwrap());
        assert!(!store_b.exists("project").await.unwrap());

        // === Test 5: Listing only shows own workspace contents ===
        let a_listing = store_a.list_dir(".").await.unwrap();
        let b_listing = store_b.list_dir(".").await.unwrap();
        assert!(a_listing.contains(&"my_notes.txt".to_string()));
        assert!(!a_listing.contains(&"secret.txt".to_string()));
        assert!(b_listing.contains(&"secret.txt".to_string()));
        assert!(!b_listing.contains(&"my_notes.txt".to_string()));

        // === Test 6: Recursive listing only shows own workspace ===
        store_a.write_file("sub/deep.txt", "deep").await.unwrap();
        let a_recursive = store_a.list_dir_recursive(".").await.unwrap();
        assert!(a_recursive.iter().any(|f| f.contains("deep.txt")));
        assert!(!a_recursive.iter().any(|f| f.contains("secret")));

        // === Test 7: Content search only within own workspace ===
        let a_search = store_a.search_content("SECRET", ".").await.unwrap();
        assert!(a_search.is_empty()); // Can't find B's secret
        let b_search = store_b.search_content("SECRET", ".").await.unwrap();
        assert!(!b_search.is_empty()); // B can find their own secret
    }

    #[tokio::test]
    async fn test_symlink_escape_blocked() {
        // Create a file outside the workspace, then try to access it via symlink
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let outside = root.path().join("outside");
        tokio::fs::create_dir_all(&workspace).await.unwrap();
        tokio::fs::create_dir_all(&outside).await.unwrap();

        // Write a file outside the workspace
        let outside_file = outside.join("secret.txt");
        tokio::fs::write(&outside_file, "OUTSIDE_SECRET").await.unwrap();

        // Create a symlink inside the workspace pointing outside
        #[cfg(unix)]
        {
            let link_path = workspace.join("escape_link");
            tokio::fs::symlink(&outside, &link_path).await.unwrap();

            let store = FileStore::new(&workspace);

            // Attempting to read through the symlink should be blocked
            // because the resolved path escapes the workspace
            let result = store.read_file("escape_link/secret.txt").await;
            assert!(result.is_err(), "Symlink escape should be blocked, but got: {:?}", result);
        }
    }

    #[tokio::test]
    async fn test_workspace_root_isolation_structure() {
        // Verify the directory structure: data_dir/{user_id}/workspace/
        // This mirrors how ws.rs and feishu.rs compute workspace_root
        let data_dir = tempfile::tempdir().unwrap();
        let data_path = data_dir.path();

        let user_a_ws = data_path.join("user_alice").join("workspace");
        let user_b_ws = data_path.join("user_bob").join("workspace");
        tokio::fs::create_dir_all(&user_a_ws).await.unwrap();
        tokio::fs::create_dir_all(&user_b_ws).await.unwrap();

        let store_a = FileStore::new(&user_a_ws);
        let store_b = FileStore::new(&user_b_ws);

        // Write to both workspaces
        store_a.write_file("project/main.py", "# Alice's code").await.unwrap();
        store_b.write_file("project/main.py", "# Bob's code").await.unwrap();

        // Each reads their own version
        let a_content = store_a.read_file("project/main.py").await.unwrap();
        let b_content = store_b.read_file("project/main.py").await.unwrap();
        assert_eq!(a_content, "# Alice's code");
        assert_eq!(b_content, "# Bob's code");
        assert_ne!(a_content, b_content);

        // User A cannot escape to user B via any file operation
        assert!(store_a.read_file("../../user_bob/workspace/project/main.py").await.is_err());
        assert!(store_a.copy_file("../../user_bob/workspace/project/main.py", "stolen.py").await.is_err());
        assert!(store_a.search_content("Bob", "../user_bob").await.is_err());
    }
}
