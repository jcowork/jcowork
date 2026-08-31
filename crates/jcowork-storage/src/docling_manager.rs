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
use std::sync::atomic::{AtomicU8, Ordering};
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
    /// Python environment bootstrap state (fresh installs).
    pub setup: SetupState,
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

/// State of the Python environment bootstrap (venv + dependencies).
///
/// On a fresh install the venv does not exist yet; the app runs
/// `scripts/setup-docling.sh` in the background so that chat and the rest of
/// the app keep working while dependencies are downloaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SetupState {
    /// The venv and dependencies are already present.
    NotNeeded,
    /// Setup has not been triggered yet.
    NotStarted,
    /// Setup script is currently running.
    Installing,
    /// Setup finished successfully.
    Done,
    /// Setup failed; it will be retried on the next trigger.
    Failed,
}

impl SetupState {
    fn as_u8(self) -> u8 {
        match self {
            SetupState::NotNeeded => 0,
            SetupState::NotStarted => 1,
            SetupState::Installing => 2,
            SetupState::Done => 3,
            SetupState::Failed => 4,
        }
    }

    fn from_u8(v: u8) -> Self {
        match v {
            0 => SetupState::NotNeeded,
            2 => SetupState::Installing,
            3 => SetupState::Done,
            4 => SetupState::Failed,
            _ => SetupState::NotStarted,
        }
    }
}

/// Manages the Docling service process lifecycle.
pub struct DoclingManager {
    /// Held while a start attempt is in progress so that concurrent callers
    /// serialise instead of spawning duplicate processes.
    start_lock: Mutex<()>,
    /// Held while the dependency setup script is running.
    setup_lock: Mutex<()>,
    /// Current [`SetupState`] (atomic so it can be read without a lock).
    setup_state: AtomicU8,
}

impl DoclingManager {
    /// Create a new manager (use [`global()`] for the shared instance).
    pub fn new() -> Self {
        Self {
            start_lock: Mutex::new(()),
            setup_lock: Mutex::new(()),
            setup_state: AtomicU8::new(SetupState::NotStarted.as_u8()),
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

        // Python environment missing? Kick off dependency setup in the
        // background and fail fast instead of hanging the caller for minutes.
        if self.setup_needed() {
            if self.setup_state() == SetupState::Installing {
                anyhow::bail!(
                    "Docling dependencies are being installed in the background; document parsing will be available in a few minutes"
                );
            }
            Self::global().spawn_setup_and_service();
            anyhow::bail!(
                "Docling dependencies are being installed in the background; document parsing will be available in a few minutes"
            );
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

    // ------------------------------------------------------------------
    // Python environment bootstrap (first-launch dependency install)
    // ------------------------------------------------------------------

    /// Current setup state. Returns [`SetupState::NotNeeded`] when the venv
    /// and dependencies are already present, regardless of stored state.
    pub fn setup_state(&self) -> SetupState {
        if !self.setup_needed() {
            return SetupState::NotNeeded;
        }
        SetupState::from_u8(self.setup_state.load(Ordering::SeqCst))
    }

    /// True when the Python venv or the setup marker is missing.
    ///
    /// Venvs created manually (before this bootstrap existed) have no marker;
    /// a fast package check (~100ms) detects them and retro-fits the marker
    /// so no redundant install is triggered.
    pub fn setup_needed(&self) -> bool {
        let Some(python) = self.find_python_venv() else {
            return true;
        };
        // python lives at <venv>/bin/python (or <venv>/Scripts/python.exe),
        // the marker sits in the venv root.
        let Some(venv_dir) = python.parent().and_then(|p| p.parent()) else {
            return true;
        };
        let marker = venv_dir.join(".docling-setup-ok");
        if marker.exists() {
            return false;
        }

        // No marker: check whether the required packages are already present
        // (hand-built venv). If so, retro-fit the marker.
        if Self::venv_has_docling_packages(&python) {
            let _ = std::fs::write(&marker, "pre-existing environment");
            info!("Detected pre-existing Docling Python environment; marker written");
            return false;
        }
        true
    }

    /// Quick (~100ms) check that docling + sentence-transformers are
    /// installed in the given interpreter.
    fn venv_has_docling_packages(python: &std::path::Path) -> bool {
        let check = r#"
from importlib.metadata import distributions
names = {d.metadata['Name'].lower().replace('-', '_') for d in distributions()}
exit(0 if 'docling' in names and 'sentence_transformers' in names else 1)
"#;
        std::process::Command::new(python)
            .args(["-c", check])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// Ensure the Python environment is set up, running the bootstrap
    /// script if necessary. Serialised by `setup_lock`; concurrent callers
    /// wait for the in-flight install instead of starting another one.
    ///
    /// NOTE: this can take several minutes on first run (pip downloads).
    /// Only await it from background tasks; request handlers should use
    /// [`spawn_setup_and_service`] and fail fast.
    pub async fn ensure_setup(&self) -> Result<()> {
        if !self.setup_needed() {
            return Ok(());
        }

        let _guard = self.setup_lock.lock().await;
        if !self.setup_needed() {
            return Ok(());
        }

        self.setup_state
            .store(SetupState::Installing.as_u8(), Ordering::SeqCst);
        match self.run_setup().await {
            Ok(()) => {
                self.setup_state.store(SetupState::Done.as_u8(), Ordering::SeqCst);
                info!("Docling dependency setup completed");
                Ok(())
            }
            Err(e) => {
                self.setup_state
                    .store(SetupState::Failed.as_u8(), Ordering::SeqCst);
                Err(e)
            }
        }
    }

    /// Start the service, installing dependencies first when missing.
    /// Used by the startup pre-warm where waiting is acceptable.
    pub async fn start_with_setup(&self) -> Result<()> {
        if self.setup_needed() {
            info!("Docling Python environment missing — installing dependencies first");
            self.ensure_setup()
                .await
                .context("Docling dependency setup failed")?;
        }
        self.start_background().await
    }

    /// Spawn a background task that sets up dependencies (if needed) and
    /// then starts the service. Returns immediately — safe for request
    /// handlers and startup paths that must not block.
    pub fn spawn_setup_and_service(&'static self) {
        tokio::spawn(async move {
            if let Err(e) = self.start_with_setup().await {
                warn!(error = %e, "Docling background setup/start failed");
            }
        });
    }

    /// Run the bootstrap script and wait for it to finish.
    /// Output is appended to `~/.jcowork/logs/docling-setup.log`.
    async fn run_setup(&self) -> Result<()> {
        let script = self
            .find_setup_script()
            .context("setup-docling script not found in any expected location")?;

        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| "/tmp".to_string());
        let log_dir = format!("{}/.jcowork/logs", home);
        std::fs::create_dir_all(&log_dir).ok();
        let log_path = format!("{}/docling-setup.log", log_dir);
        let log_file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .context("Failed to open docling-setup log file")?;
        let err_file = log_file.try_clone().context("Failed to clone log file")?;

        let mut cmd = if script.extension().is_some_and(|e| e == "ps1") {
            let mut c = std::process::Command::new("powershell");
            c.args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"]);
            c.arg(&script);
            c
        } else {
            let mut c = std::process::Command::new("bash");
            c.arg(&script);
            c
        };

        info!(script = %script.display(), "Running Docling dependency setup");
        let mut child = cmd
            .stdout(std::process::Stdio::from(log_file))
            .stderr(std::process::Stdio::from(err_file))
            .spawn()
            .context("Failed to spawn setup script")?;

        // Poll instead of blocking a runtime thread.
        loop {
            match child.try_wait().context("Failed to poll setup script")? {
                Some(status) if status.success() => return Ok(()),
                Some(status) => {
                    anyhow::bail!(
                        "Docling dependency setup failed (exit {}); see {}",
                        status,
                        log_path
                    );
                }
                None => tokio::time::sleep(std::time::Duration::from_secs(2)).await,
            }
        }
    }

    /// Locate the bundled bootstrap script (`scripts/setup-docling.sh|ps1`).
    fn find_setup_script(&self) -> Option<PathBuf> {
        let name = if cfg!(windows) {
            "scripts/setup-docling.ps1"
        } else {
            "scripts/setup-docling.sh"
        };
        self.find_resource_path(name)
    }

    /// Find a project-relative resource file, checking Tauri bundle layouts
    /// first, then dev/repo layouts relative to the executable and cwd.
    fn find_resource_path(&self, rel_path: &str) -> Option<PathBuf> {
        let mut searched = Vec::new();

        if let Some(exe) = std::env::current_exe().ok() {
            if let Some(exe_dir) = exe.parent() {
                if let Some(contents) = exe_dir.parent() {
                    let res = contents.join("Resources");
                    // Tauri encodes ".." as "_up_" in resource paths
                    for base in [
                        res.join("_up_/_up_"),
                        res.clone(),
                    ] {
                        let p = base.join(rel_path);
                        searched.push(p.clone());
                        if p.exists() {
                            return Some(p);
                        }
                    }
                }
                // Dev build: exe at target/{profile}/jcowork-desktop
                if let Some(project_root) = exe_dir.parent().and_then(|p| p.parent()) {
                    let p = project_root.join(rel_path);
                    searched.push(p.clone());
                    if p.exists() {
                        return Some(p);
                    }
                }
            }
        }

        if let Ok(cwd) = std::env::current_dir() {
            let p = cwd.join(rel_path);
            searched.push(p.clone());
            if p.exists() {
                return Some(p);
            }
            let mut dir = cwd.as_path();
            while let Some(parent) = dir.parent() {
                let p = parent.join(rel_path);
                searched.push(p.clone());
                if p.exists() {
                    return Some(p);
                }
                dir = parent;
            }
        }

        warn!(
            rel_path,
            searched = ?searched.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
            "Resource not found in any expected location"
        );
        None
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
        let setup = self.setup_state();
        let message = if running {
            "Docling service is running".to_string()
        } else if setup == SetupState::Installing {
            "Docling dependencies are being installed in the background".to_string()
        } else {
            "Docling service is not running".to_string()
        };
        DoclingStatus {
            running,
            starting: !running && self.start_lock.try_lock().is_err(),
            service_url: url,
            message,
            setup,
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
            // Includes dependency bootstrap on fresh installs — acceptable
            // here because pre-warm runs as a detached background task.
            let fut = self.start_with_setup();
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

    #[test]
    fn setup_state_u8_roundtrip() {
        for s in [
            SetupState::NotNeeded,
            SetupState::NotStarted,
            SetupState::Installing,
            SetupState::Done,
            SetupState::Failed,
        ] {
            assert_eq!(SetupState::from_u8(s.as_u8()), s);
        }
    }

    #[test]
    fn setup_state_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&SetupState::Installing).unwrap(),
            "\"installing\""
        );
        assert_eq!(
            serde_json::to_string(&SetupState::NotNeeded).unwrap(),
            "\"not_needed\""
        );
        assert_eq!(
            serde_json::to_string(&SetupState::Failed).unwrap(),
            "\"failed\""
        );
    }

    #[test]
    fn setup_needed_depends_on_venv_and_marker() {
        // Fresh "machine": empty HOME-like directory -> no venv -> needed.
        let tmp = tempfile::tempdir().expect("tempdir");
        let saved_home = std::env::var("HOME").ok();
        // SAFETY: single-threaded test; restored before returning. Kept in a
        // separate #[test] that does not touch HOME anywhere else.
        unsafe { std::env::set_var("HOME", tmp.path()) };

        let manager = DoclingManager::new();
        assert!(manager.setup_needed(), "no venv => setup needed");
        assert_eq!(manager.setup_state(), SetupState::NotStarted);

        // venv python present but no marker -> still needed.
        let venv_bin = tmp.path().join(".jcowork/venv/bin");
        std::fs::create_dir_all(&venv_bin).unwrap();
        std::fs::write(venv_bin.join("python"), "").unwrap();
        assert!(manager.setup_needed(), "no marker => setup needed");

        // Marker present -> not needed, state reports NotNeeded.
        std::fs::write(tmp.path().join(".jcowork/venv/.docling-setup-ok"), "")
            .unwrap();
        assert!(!manager.setup_needed(), "marker => setup done");
        assert_eq!(manager.setup_state(), SetupState::NotNeeded);

        // Restore HOME.
        match saved_home {
            Some(h) => unsafe { std::env::set_var("HOME", h) },
            None => unsafe { std::env::remove_var("HOME") },
        }
    }

    #[test]
    fn find_setup_script_locates_repo_script() {
        // Tests run with cwd inside the workspace, so walking up from cwd
        // must find scripts/setup-docling.sh in the repo root.
        let manager = DoclingManager::new();
        let script = manager.find_setup_script();
        assert!(
            script.is_some(),
            "setup-docling script must be discoverable from the workspace"
        );
        let path = script.unwrap();
        assert!(
            path.to_string_lossy().contains("scripts"),
            "unexpected path: {}",
            path.display()
        );
    }
}
