//! Docling service lifecycle manager.
//!
//! Manages auto-starting the Docling Python service (PDF parsing & embeddings)
//! when it is not already running.  This is primarily used by the desktop app
//! where the service is not started alongside the main server.
//!
//! The manager is accessed through a process-wide singleton ([`DoclingManager::global`]).

use anyhow::{Context, Result};
use std::path::PathBuf;
use std::sync::LazyLock;
use tokio::sync::Mutex;
use tracing::{info, warn};

/// Process-wide global Docling service manager.
static DOCLING_MANAGER: LazyLock<DoclingManager> = LazyLock::new(DoclingManager::new);

/// Status of the Docling service as reported by the API.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DoclingStatus {
    pub running: bool,
    pub starting: bool,
    pub service_url: String,
    pub message: String,
}

/// Manages the Docling service process lifecycle.
pub struct DoclingManager {
    /// Held while a start attempt is in progress so that concurrent callers
    /// serialise instead of spawning duplicate processes.
    start_lock: Mutex<()>,
}

impl DoclingManager {
    /// Create a new manager (use [`global()`] for the shared instance).
    pub fn new() -> Self {
        Self {
            start_lock: Mutex::new(()),
        }
    }

    /// Access the process-wide singleton.
    pub fn global() -> &'static DoclingManager {
        &DOCLING_MANAGER
    }

    /// Base URL of the Docling service (from env or default).
    pub fn service_url() -> String {
        std::env::var("DOCLING_SERVICE_URL")
            .unwrap_or_else(|_| "http://localhost:50060".to_string())
    }

    // ------------------------------------------------------------------
    // Public API
    // ------------------------------------------------------------------

    /// Quick health check — does the service respond on `/health`?
    pub async fn is_healthy(&self) -> bool {
        Self::check_health(&Self::service_url()).await
    }

    /// Ensure the Docling service is running, starting it if necessary.
    ///
    /// Blocks until the service is healthy or a timeout is reached.
    /// Safe to call concurrently — only one caller will actually spawn
    /// the process; others wait for it to become healthy.
    pub async fn ensure_running(&self) -> Result<()> {
        // Fast path: already running.
        if Self::check_health(&Self::service_url()).await {
            return Ok(());
        }

        // Slow path: acquire the start lock (serialises concurrent callers).
        let _guard = self.start_lock.lock().await;

        // Another caller may have started it while we waited for the lock.
        if Self::check_health(&Self::service_url()).await {
            return Ok(());
        }

        // Locate prerequisites.
        let python = self.find_python_venv()
            .context("Python venv not found at ~/.jcowork/venv — run scripts/install.sh first")?;
        let app_dir = self.find_docling_app()
            .context("Docling app.py not found — ensure services/docling/ is available")?;

        info!(
            python = %python.display(),
            app_dir = %app_dir.display(),
            "Starting Docling service"
        );

        self.spawn_service(&python, &app_dir)?;

        // Wait for healthy (up to 180 s — first run downloads models).
        let timeout = std::time::Duration::from_secs(180);
        let poll = std::time::Duration::from_secs(3);
        let deadline = tokio::time::Instant::now() + timeout;

        while tokio::time::Instant::now() < deadline {
            if Self::check_health(&Self::service_url()).await {
                info!("Docling service is healthy");
                return Ok(());
            }
            tokio::time::sleep(poll).await;
        }

        anyhow::bail!(
            "Docling service did not become healthy within {}s",
            timeout.as_secs()
        );
    }

    /// Start the service in the background and return immediately.
    ///
    /// The caller can poll [`is_healthy()`] or use [`wait_healthy()`] to
    /// track progress.
    pub async fn start_background(&self) -> Result<()> {
        if Self::check_health(&Self::service_url()).await {
            return Ok(());
        }

        let _guard = self.start_lock.lock().await;

        if Self::check_health(&Self::service_url()).await {
            return Ok(());
        }

        let python = match self.find_python_venv() {
            Some(p) => p,
            None => {
                let err = anyhow::anyhow!("Python venv not found at ~/.jcowork/venv — run scripts/install.sh first");
                warn!("{}", err);
                return Err(err);
            }
        };
        let app_dir = match self.find_docling_app() {
            Some(p) => p,
            None => {
                let err = anyhow::anyhow!("Docling app.py not found in any expected location");
                warn!("{}", err);
                return Err(err);
            }
        };

        info!(
            python = %python.display(),
            app_dir = %app_dir.display(),
            "Starting Docling service in background"
        );

        self.spawn_service(&python, &app_dir)?;
        Ok(())
    }

    /// Poll `/health` until the service is healthy or timeout.
    pub async fn wait_healthy(
        &self,
        timeout: std::time::Duration,
    ) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        let poll = std::time::Duration::from_secs(3);
        while tokio::time::Instant::now() < deadline {
            if Self::check_health(&Self::service_url()).await {
                return true;
            }
            tokio::time::sleep(poll).await;
        }
        false
    }

    /// Build a [`DoclingStatus`] snapshot for the API.
    pub async fn status(&self) -> DoclingStatus {
        let url = Self::service_url();
        let running = Self::check_health(&url).await;
        DoclingStatus {
            running,
            starting: !running && self.start_lock.try_lock().is_err(),
            service_url: url,
            message: if running {
                "Docling service is running".to_string()
            } else {
                "Docling service is not running".to_string()
            },
        }
    }

    // ------------------------------------------------------------------
    // Internals
    // ------------------------------------------------------------------

    /// Check if the Docling `/health` endpoint responds.
    async fn check_health(service_url: &str) -> bool {
        let client = match reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(3))
            .build()
        {
            Ok(c) => c,
            Err(_) => return false,
        };
        match client.get(format!("{}/health", service_url)).send().await {
            Ok(resp) => resp.status().is_success(),
            Err(_) => false,
        }
    }

    /// Spawn the Docling uvicorn process.
    fn spawn_service(&self, python: &PathBuf, app_dir: &PathBuf) -> Result<()> {
        let home = std::env::var("HOME")
            .unwrap_or_else(|_| "/tmp".to_string());
        let assets_dir = format!("{}/.jcowork/data/docling_assets", home);
        let log_dir = format!("{}/.jcowork/logs", home);
        std::fs::create_dir_all(&log_dir).ok();

        let log_path = format!("{}/docling.log", log_dir);
        let log_file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .context("Failed to open docling log file")?;
        let err_file = log_file.try_clone().context("Failed to clone log file")?;

        let child = std::process::Command::new(python)
            .args([
                "-m", "uvicorn", "app:app",
                "--host", "127.0.0.1",
                "--port", "50060",
            ])
            .current_dir(app_dir)
            .env("ASSETS_DIR", &assets_dir)
            .env("PORT", "50060")
            .stdout(std::process::Stdio::from(log_file))
            .stderr(std::process::Stdio::from(err_file))
            .spawn()
            .context("Failed to spawn Docling service process")?;

        info!(pid = child.id(), "Docling service process spawned");
        // We intentionally do not wait on the child — it runs as a daemon.
        Ok(())
    }

    /// Find the Python venv binary.
    ///
    /// Search order:
    /// 1. `~/.jcowork/venv/bin/python`  (macOS/Linux standard)
    /// 2. `~/.jcowork/venv/Scripts/python.exe` (Windows)
    fn find_python_venv(&self) -> Option<PathBuf> {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| "/tmp".to_string());

        let candidates = [
            PathBuf::from(&home).join(".jcowork/venv/bin/python"),
            PathBuf::from(&home).join(".jcowork/venv/Scripts/python.exe"),
        ];

        candidates.into_iter().find(|p| p.exists())
    }

    /// Find the directory containing `app.py` for the Docling service.
    ///
    /// Tauri encodes `..` in resource paths as `_up_`, so
    /// `../../services/docling/app.py` becomes `_up_/_up_/services/docling/app.py`
    /// inside the resource directory.
    ///
    /// Search order:
    /// 1. Tauri resource dir (encoded) — `Resources/_up_/_up_/services/docling/`
    /// 2. Tauri resource dir (flat)    — `Resources/app.py`
    /// 3. Tauri resource dir (nested)  — `Resources/services/docling/`
    /// 4. Walk up from executable      — `exe/../../services/docling/`
    /// 5. Current working directory    — `./services/docling/`
    /// 6. Walk up from cwd
    fn find_docling_app(&self) -> Option<PathBuf> {
        let mut searched = Vec::new();

        if let Some(exe) = std::env::current_exe().ok() {
            if let Some(exe_dir) = exe.parent() {
                // macOS Tauri bundle: exe in Contents/MacOS, resources in Contents/Resources
                if let Some(contents) = exe_dir.parent() {
                    let res = contents.join("Resources");

                    // Tauri encodes ".." as "_up_" in resource paths
                    let encoded = res.join("_up_/_up_/services/docling");
                    searched.push(encoded.clone());
                    if encoded.join("app.py").exists() {
                        info!(path = %encoded.display(), "Found Docling app.py (Tauri encoded path)");
                        return encoded.into();
                    }

                    // Flat placement
                    let flat = res.clone();
                    searched.push(flat.clone());
                    if flat.join("app.py").exists() {
                        info!(path = %flat.display(), "Found Docling app.py (flat resource)");
                        return flat.into();
                    }

                    // Nested placement
                    let nested = res.join("services/docling");
                    searched.push(nested.clone());
                    if nested.join("app.py").exists() {
                        info!(path = %nested.display(), "Found Docling app.py (nested resource)");
                        return nested.into();
                    }
                }

                // Walk up from executable dir (project root for dev builds)
                // exe is at target/debug/jcowork-desktop → project root is exe/../../
                if let Some(project_root) = exe_dir.parent().and_then(|p| p.parent()) {
                    let d = project_root.join("services/docling");
                    searched.push(d.clone());
                    if d.join("app.py").exists() {
                        info!(path = %d.display(), "Found Docling app.py (project root from exe)");
                        return d.into();
                    }
                }
            }
        }

        // Current working directory
        if let Ok(cwd) = std::env::current_dir() {
            let d = cwd.join("services/docling");
            searched.push(d.clone());
            if d.join("app.py").exists() {
                info!(path = %d.display(), "Found Docling app.py (cwd)");
                return d.into();
            }

            // Walk up from cwd
            let mut dir = cwd.as_path();
            while let Some(parent) = dir.parent() {
                let d = parent.join("services/docling");
                searched.push(d.clone());
                if d.join("app.py").exists() {
                    info!(path = %d.display(), "Found Docling app.py (walk up from cwd)");
                    return d.into();
                }
                dir = parent;
            }
        }

        // Log all searched paths for debugging
        warn!(
            searched = ?searched.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
            "Docling app.py not found in any expected location"
        );
        None
    }
}
