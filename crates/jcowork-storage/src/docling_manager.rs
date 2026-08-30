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

/// Outcome of a [`DoclingManager::prewarm`] attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreWarmStatus {
    /// The service was already healthy before pre-warm started.
    AlreadyRunning,
    /// The service was started (or starting) and became healthy.
    Ready,
    /// The service could not be started or did not become healthy in time.
    /// Callers may fall back to on-demand startup.
    Failed,
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

    /// Pre-warm the service: after `delay`, ensure it is running and wait
    /// until it reports healthy (up to `wait_timeout`).
    ///
    /// Designed to be spawned as a background task at desktop-app startup so
    /// the service is ready before the first document upload. Never fails
    /// hard — returns [`PreWarmStatus`] so on-demand startup can retry.
    pub async fn prewarm(
        &self,
        delay: std::time::Duration,
        wait_timeout: std::time::Duration,
    ) -> PreWarmStatus {
        self.prewarm_with_url(
            &Self::service_url(),
            delay,
            wait_timeout,
            std::time::Duration::from_secs(3),
        )
        .await
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

    /// Pre-warm implementation against an explicit service URL (testable).
    async fn prewarm_with_url(
        &self,
        service_url: &str,
        delay: std::time::Duration,
        wait_timeout: std::time::Duration,
        poll_interval: std::time::Duration,
    ) -> PreWarmStatus {
        let start_fn = || {
            let fut = self.start_background();
            async move { fut.await }
        };
        Self::prewarm_core(service_url, delay, wait_timeout, poll_interval, start_fn).await
    }

    /// Core pre-warm state machine: delay → health check → start → wait.
    ///
    /// The start action is injected so unit tests can verify the polling
    /// logic without spawning real processes.
    async fn prewarm_core<F, Fut>(
        service_url: &str,
        delay: std::time::Duration,
        wait_timeout: std::time::Duration,
        poll_interval: std::time::Duration,
        start_fn: F,
    ) -> PreWarmStatus
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<()>>,
    {
        tokio::time::sleep(delay).await;

        if Self::check_health(service_url).await {
            info!(%service_url, "Docling pre-warm: service already running");
            return PreWarmStatus::AlreadyRunning;
        }

        if let Err(e) = start_fn().await {
            warn!(error = %e, "Docling pre-warm failed to start the service");
            return PreWarmStatus::Failed;
        }

        // Poll until healthy — first run may download models, so callers
        // should allow a generous `wait_timeout`.
        let deadline = tokio::time::Instant::now() + wait_timeout;
        while tokio::time::Instant::now() < deadline {
            if Self::check_health(service_url).await {
                info!(%service_url, "Docling pre-warm complete, service is ready");
                return PreWarmStatus::Ready;
            }
            tokio::time::sleep(poll_interval).await;
        }

        warn!(
            timeout_secs = wait_timeout.as_secs(),
            "Docling service did not become healthy during pre-warm; it will be retried on demand"
        );
        PreWarmStatus::Failed
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    fn ms(n: u64) -> std::time::Duration {
        std::time::Duration::from_millis(n)
    }

    /// Spawn a minimal HTTP server on a random port. Every request gets
    /// `200 OK` while `healthy` is true, `503` otherwise. Returns the base
    /// URL and a connection counter.
    async fn spawn_mock_health_server(
        healthy: Arc<AtomicBool>,
    ) -> (String, Arc<AtomicUsize>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock server");
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(AtomicUsize::new(0));
        let req_count = requests.clone();

        tokio::spawn(async move {
            loop {
                let (mut stream, _) = match listener.accept().await {
                    Ok(s) => s,
                    Err(_) => break,
                };
                let healthy = healthy.clone();
                let req_count = req_count.clone();
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = [0u8; 1024];
                    let _ = stream.read(&mut buf).await;
                    req_count.fetch_add(1, Ordering::SeqCst);
                    let (status, body) = if healthy.load(Ordering::SeqCst) {
                        ("200 OK", r#"{"status":"ok"}"#)
                    } else {
                        ("503 Service Unavailable", r#"{"status":"starting"}"#)
                    };
                    let resp = format!(
                        "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        status,
                        body.len(),
                        body
                    );
                    let _ = stream.write_all(resp.as_bytes()).await;
                });
            }
        });

        (url, requests)
    }

    #[tokio::test]
    async fn prewarm_returns_already_running_when_healthy() {
        let (url, requests) =
            spawn_mock_health_server(Arc::new(AtomicBool::new(true))).await;
        let started = Arc::new(AtomicUsize::new(0));
        let s = started.clone();

        let status = DoclingManager::prewarm_core(&url, ms(0), ms(500), ms(50), move || {
            s.fetch_add(1, Ordering::SeqCst);
            async { Ok(()) }
        })
        .await;

        assert_eq!(status, PreWarmStatus::AlreadyRunning);
        assert_eq!(
            started.load(Ordering::SeqCst),
            0,
            "start must not be called when the service is already healthy"
        );
        assert!(requests.load(Ordering::SeqCst) >= 1);
    }

    #[tokio::test]
    async fn prewarm_starts_service_and_waits_until_healthy() {
        let healthy = Arc::new(AtomicBool::new(false));
        let (url, requests) = spawn_mock_health_server(healthy.clone()).await;

        // Simulate the start action flipping the service to healthy.
        let h = healthy.clone();
        let status = DoclingManager::prewarm_core(
            &url,
            ms(0),
            std::time::Duration::from_secs(5),
            ms(50),
            move || {
                h.store(true, Ordering::SeqCst);
                async { Ok(()) }
            },
        )
        .await;

        assert_eq!(status, PreWarmStatus::Ready);
        assert!(
            requests.load(Ordering::SeqCst) >= 2,
            "expected an initial unhealthy probe plus at least one poll"
        );
    }

    #[tokio::test]
    async fn prewarm_fails_when_service_never_becomes_healthy() {
        let (url, _requests) =
            spawn_mock_health_server(Arc::new(AtomicBool::new(false))).await;

        let started_at = std::time::Instant::now();
        let status = DoclingManager::prewarm_core(&url, ms(0), ms(300), ms(50), || async { Ok(()) }).await;

        assert_eq!(status, PreWarmStatus::Failed);
        assert!(
            started_at.elapsed() >= ms(300),
            "must poll until the wait timeout elapses"
        );
    }

    #[tokio::test]
    async fn prewarm_fails_fast_when_start_errors() {
        let (url, _requests) =
            spawn_mock_health_server(Arc::new(AtomicBool::new(false))).await;

        let started_at = std::time::Instant::now();
        let status = DoclingManager::prewarm_core(
            &url,
            ms(0),
            std::time::Duration::from_secs(30),
            ms(50),
            || async { Err::<(), _>(anyhow::anyhow!("venv not found")) },
        )
        .await;

        assert_eq!(status, PreWarmStatus::Failed);
        assert!(
            started_at.elapsed() < std::time::Duration::from_secs(5),
            "a start error must fail immediately, not wait out the timeout"
        );
    }

    #[tokio::test]
    async fn prewarm_honours_initial_delay() {
        let (url, _requests) =
            spawn_mock_health_server(Arc::new(AtomicBool::new(true))).await;

        let started_at = std::time::Instant::now();
        let status = DoclingManager::prewarm_core(&url, ms(200), ms(500), ms(50), || async { Ok(()) }).await;

        assert_eq!(status, PreWarmStatus::AlreadyRunning);
        assert!(
            started_at.elapsed() >= ms(200),
            "the configured delay must elapse before the first health probe"
        );
    }
}
