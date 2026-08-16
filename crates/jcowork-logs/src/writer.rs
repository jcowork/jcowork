//! Async log writer with date-based and size-based rotation.

use anyhow::Result;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::models::LogEntry;

/// Maximum log file size before rotation (1 GB).
const MAX_FILE_SIZE: u64 = 1_000_000_000;

/// Async log writer that writes JSON Lines entries to files.
///
/// Files are stored under `log_dir` with the naming convention:
/// - `YYYY-MM-DD.jsonl` (first file of the day)
/// - `YYYY-MM-DD-001.jsonl` (after first rotation)
/// - `YYYY-MM-DD-002.jsonl` (after second rotation)
/// - etc.
///
/// Rotation happens when:
/// 1. The date changes (new day = new file)
/// 2. The current file exceeds 1 GB
pub struct LogWriter {
    inner: Arc<Mutex<LogWriterInner>>,
}

struct LogWriterInner {
    log_dir: PathBuf,
    current_file: Option<tokio::fs::File>,
    current_date: String,
    current_suffix: u32,
    current_size: u64,
}

impl LogWriter {
    /// Create a new LogWriter, creating the log directory if it doesn't exist.
    pub async fn new(log_dir: PathBuf) -> Result<Self> {
        tokio::fs::create_dir_all(&log_dir).await?;
        tracing::info!(dir = %log_dir.display(), "Log directory initialized");
        Ok(Self {
            inner: Arc::new(Mutex::new(LogWriterInner {
                log_dir,
                current_file: None,
                current_date: String::new(),
                current_suffix: 0,
                current_size: 0,
            })),
        })
    }

    /// Create a no-op LogWriter that silently discards all entries.
    /// Used as fallback when the real log directory cannot be created.
    pub fn new_disabled() -> Self {
        Self {
            inner: Arc::new(Mutex::new(LogWriterInner {
                log_dir: PathBuf::from("/dev/null"),
                current_file: None,
                current_date: String::new(),
                current_suffix: 0,
                current_size: 0,
            })),
        }
    }

    /// Write a log entry. Thread-safe via internal mutex.
    pub async fn write(&self, entry: &LogEntry) {
        let mut inner = self.inner.lock().await;
        if let Err(e) = inner.write_entry(entry).await {
            tracing::warn!(error = %e, "Failed to write log entry");
        }
    }
}

impl LogWriterInner {
    /// Get today's date string (UTC).
    fn today() -> String {
        chrono::Utc::now().format("%Y-%m-%d").to_string()
    }

    /// Get the current log file path based on date and suffix.
    fn file_path(&self) -> PathBuf {
        if self.current_suffix == 0 {
            self.log_dir.join(format!("{}.jsonl", self.current_date))
        } else {
            self.log_dir.join(format!("{}-{:03}.jsonl", self.current_date, self.current_suffix))
        }
    }

    /// Ensure we have an open file handle for the current date.
    /// Handles date rotation and size rotation.
    async fn ensure_file(&mut self) -> Result<()> {
        let today = Self::today();

        // Date changed or no file open yet
        if today != self.current_date || self.current_file.is_none() {
            self.current_date = today;
            self.current_suffix = 0;
            self.current_size = 0;
            let path = self.file_path();
            // Check existing file size to resume correctly
            if path.exists() {
                let metadata = tokio::fs::metadata(&path).await?;
                self.current_size = metadata.len();
                // If existing file is already over limit, increment suffix
                if self.current_size >= MAX_FILE_SIZE {
                    self.current_suffix += 1;
                    self.current_size = 0;
                }
            }
            let path = self.file_path();
            let file = tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .await?;
            self.current_file = Some(file);
            return Ok(());
        }

        // Same date, check size rotation
        if self.current_size >= MAX_FILE_SIZE {
            self.current_suffix += 1;
            self.current_size = 0;
            let path = self.file_path();
            let file = tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .await?;
            self.current_file = Some(file);
        }

        Ok(())
    }

    /// Write a single log entry as a JSON line.
    async fn write_entry(&mut self, entry: &LogEntry) -> Result<()> {
        self.ensure_file().await?;

        let mut line = serde_json::to_string(entry)?;
        line.push('\n');

        let bytes = line.as_bytes();
        let file = self.current_file.as_mut().unwrap();
        use tokio::io::AsyncWriteExt;
        file.write_all(bytes).await?;
        file.flush().await?;

        self.current_size += bytes.len() as u64;

        Ok(())
    }
}
