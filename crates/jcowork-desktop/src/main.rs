//! Jcowork Desktop — Tauri v2 桌面应用
//!
//! 启动流程:
//! 1. 初始化 tracing + 加载 .env（优先从 app bundle 资源目录）
//! 2. 复用 jcowork-server 的初始化逻辑创建 AppState + Axum app
//! 3. 在后台 tokio task 中启动 Axum 服务器 (localhost:3000)
//! 4. 等待服务器就绪后，通过 Tauri 打开窗口加载 http://localhost:3000

use std::sync::Arc;
use std::sync::RwLock;
use tower_http::cors::CorsLayer;
use tracing::{error, info, warn};

use jcowork_gateway::{
    auth::AuthConfig,
    router::{self, AppState},
    session::SessionManager,
};
use jcowork_logs::LogWriter;
use jcowork_memory::{BuiltinMemoryProvider, MemoryManager};
use jcowork_server::config::ServerConfig;
use jcowork_skills::SkillManager;
use jcowork_storage::FeishuConfigStore;

/// Resolve the Tauri resource directory relative to the current executable.
///
/// macOS bundle layout:
///   Jcowork.app/Contents/MacOS/jcowork-desktop   ← current_exe
///   Jcowork.app/Contents/Resources/               ← resource_dir
fn resolve_resource_dir() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    // Go up from MacOS/ to Contents/, then into Resources/
    exe.parent()?
        .parent()?
        .join("Resources")
        .into()
}

/// Try to load .env from the app bundle resource directory.
fn load_env_from_resources() {
    if let Some(res_dir) = resolve_resource_dir() {
        let env_path = res_dir.join(".env");
        if env_path.exists() {
            if let Err(e) = dotenvy::from_path(&env_path) {
                warn!(path = %env_path.display(), error = %e, "Failed to load .env from resources");
            } else {
                info!(path = %env_path.display(), "Loaded .env from app resources");
            }
            return;
        }
    }
    // Fallback: try default locations
    let _ = dotenvy::dotenv();
}

/// Find providers.json — first in resource dir, then standard paths.
fn find_providers_json() -> Option<String> {
    if let Some(res_dir) = resolve_resource_dir() {
        let path = res_dir.join("providers.json");
        if path.exists() {
            return path.to_str().map(|s| s.to_string());
        }
    }
    // Fallback: check standard paths
    for p in &["providers.json", "config/providers.json"] {
        if std::path::Path::new(p).exists() {
            return Some(p.to_string());
        }
    }
    None
}

/// Set JCWORK_WEB_DIR to the bundled frontend resources so the Axum server
/// can find the static files when running from the app bundle.
fn set_web_dir_from_resources() {
    if std::env::var("JCWORK_WEB_DIR").is_ok() {
        return; // Already set externally
    }
    if let Some(res_dir) = resolve_resource_dir() {
        // Tauri encodes ".." as "_up_" in resource paths
        let candidates = [
            res_dir.join("web").join("dist"),
            res_dir.join("_up_").join("_up_").join("web").join("dist"),
        ];
        for web_dist in &candidates {
            if web_dist.exists() {
                // SAFETY: We set this env var once at startup before any threads are spawned.
                unsafe { std::env::set_var("JCWORK_WEB_DIR", web_dist.to_str().unwrap_or("")); }
                info!(path = %web_dist.display(), "Set JCWORK_WEB_DIR from app resources");
                return;
            }
        }
    }
}

/// Set JCWORK_SCRIPTS_DIR to the bundled scripts directory.
fn set_scripts_dir_from_resources() {
    if std::env::var("JCWORK_SCRIPTS_DIR").is_ok() {
        return;
    }
    if let Some(res_dir) = resolve_resource_dir() {
        // Check for web_search.py in resources
        let candidates = [
            res_dir.join("web_search.py"),
            res_dir.join("_up_").join("_up_").join("web_search.py"),
        ];
        for script in &candidates {
            if script.exists() {
                // Set the parent directory as scripts dir
                if let Some(parent) = script.parent() {
                    unsafe { std::env::set_var("JCWORK_SCRIPTS_DIR", parent.to_str().unwrap_or("")); }
                    info!(path = %parent.display(), "Set JCWORK_SCRIPTS_DIR from app resources");
                }
                return;
            }
        }
    }
}

#[tokio::main]
async fn main() {
    // ── 1. Load .env ──
    load_env_from_resources();

    // ── 2. Set web dir from bundle resources ──
    set_web_dir_from_resources();

    // ── 4. Set scripts dir from bundle resources ──
    set_scripts_dir_from_resources();

    // ── 2. Initialize tracing ──
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "jcowork=info".parse().unwrap()),
        )
        .init();

    info!("Starting Jcowork Desktop");

    // ── 3. Load configuration ──
    let config = ServerConfig::from_env();
    info!(
        ?config.host,
        ?config.port,
        ?config.data_dir,
        ?config.default_model,
        "Server configuration loaded"
    );

    // ── 4. Create data directory ──
    let data_dir = shellexpand::tilde(&config.data_dir).to_string();
    if let Err(e) = tokio::fs::create_dir_all(&data_dir).await {
        error!(error = %e, "Failed to create data directory");
        show_error_and_exit(&format!("无法创建数据目录: {}\n\n{}", data_dir, e));
        return;
    }

    // ── 5. Initialize LLM router ──
    let llm_router = match find_providers_json() {
        Some(path) => {
            info!(path = %path, "Loading providers.json from resource path");
            match jcowork_llm::LlmRouter::from_env_with_config_path(Some(&path)) {
                Ok(r) => r,
                Err(e) => {
                    warn!(error = %e, "Failed to load providers from {}, using empty router", path);
                    jcowork_llm::LlmRouter::new()
                }
            }
        }
        None => {
            warn!("providers.json not found, using empty LLM router");
            jcowork_llm::LlmRouter::new()
        }
    };
    info!(
        providers = ?llm_router.available_providers(),
        "LLM providers registered"
    );

    // Migrate providers to persistent file on first run (same as server)
    let providers_file = jcowork_llm::LlmRouter::providers_file_path(&data_dir);
    if !std::path::Path::new(&providers_file).exists() {
        // Load bundled providers.json from Tauri resources
        let bundled_configs = find_providers_json()
            .and_then(|path| jcowork_llm::LlmRouter::load_provider_configs_from_path(&path).ok())
            .unwrap_or_default();

        let entries: Vec<jcowork_llm::ProviderEntry> = bundled_configs.into_iter().map(|c| {
            let api_key = if c.env_key.is_empty() {
                String::new()
            } else {
                std::env::var(&c.env_key).unwrap_or_default()
            };
            let base_url_env = format!("{}_BASE_URL", c.id.to_uppercase());
            let base_url = std::env::var(&base_url_env).unwrap_or_else(|_| c.base_url.clone());
            jcowork_llm::ProviderEntry {
                id: c.id,
                name: c.name,
                api_key,
                base_url,
                default_model: c.default_model,
                context_length: c.context_length,
                models: c.models,
            }
        }).collect();

        if !entries.is_empty() {
            if let Err(e) = jcowork_llm::LlmRouter::save_entries_to_file(&providers_file, &entries) {
                warn!(error = %e, "Failed to save initial providers to file");
            } else {
                info!(path = %providers_file, count = entries.len(), "Migrated providers to persistent file");
            }
        }
    }

    // Load router from persistent file if available
    let llm_router = if std::path::Path::new(&providers_file).exists() {
        match jcowork_llm::LlmRouter::load_entries_from_file(&providers_file) {
            Ok(entries) => {
                let router = jcowork_llm::LlmRouter::rebuild_from_entries(&entries);
                info!(providers = ?router.available_providers(), "LLM router loaded from persistent file");
                router
            }
            Err(e) => {
                warn!(error = %e, "Failed to load from providers file, using env-based router");
                llm_router
            }
        }
    } else {
        llm_router
    };

    // ── 6. Initialize memory provider (SQLite) ──
    let db_path = format!("{}/jcowork.db", data_dir);
    let pool = match sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(10)
        .connect(&format!("sqlite:{}?mode=rwc", db_path))
        .await
    {
        Ok(p) => p,
        Err(e) => {
            error!(error = %e, "Failed to connect to SQLite");
            show_error_and_exit(&format!("无法连接数据库: {}\n\n{}", db_path, e));
            return;
        }
    };

    if let Err(e) = jcowork_storage::migration::run_migrations(&pool).await {
        error!(error = %e, "Failed to run database migrations");
        show_error_and_exit(&format!("数据库迁移失败:\n\n{}", e));
        return;
    }

    let memory_provider = BuiltinMemoryProvider::new(pool.clone());
    if let Err(e) = memory_provider.init().await {
        warn!(error = %e, "Memory provider init failed, continuing without memory");
    }
    info!(db = %db_path, "Memory database initialized");

    let mut memory_manager = MemoryManager::new();
    memory_manager.add_provider(Arc::new(memory_provider));
    let memory_manager = Arc::new(memory_manager);

    // ── 7. Initialize skill manager ──
    let skill_manager = Arc::new(SkillManager::new(pool.clone()));
    info!("Skill manager initialized");

    // ── 8. Initialize cron scheduler ──
    let cron_scheduler = Arc::new(jcowork_cron::CronScheduler::new());

    // ── 9. Initialize user store ──
    let user_store = match jcowork_storage::UserStore::new(&data_dir).await {
        Ok(s) => Arc::new(s),
        Err(e) => {
            error!(error = %e, "Failed to initialize user store");
            show_error_and_exit(&format!("用户存储初始化失败:\n\n{}", e));
            return;
        }
    };
    info!("User store initialized");

    // ── 10. Initialize log writer ──
    let log_dir = format!("{}/logs", data_dir);
    let log_writer = match LogWriter::new(log_dir.into()).await {
        Ok(w) => Arc::new(w),
        Err(e) => {
            warn!(error = %e, "Log writer init failed, continuing without logging");
            Arc::new(LogWriter::new_disabled())
        }
    };
    info!("Log writer initialized");

    // ── 11. Initialize tool registry ──
    let tool_registry = jcowork_gateway::ws::build_tool_registry(
        cron_scheduler.clone(),
        memory_manager.clone(),
        log_writer.clone(),
    );

    // ── 12. Initialize session manager ──
    let session_manager = Arc::new(SessionManager::new());

    // ── 13. Build app state ──
    let auth_config = AuthConfig {
        jwt_secret: config.jwt_secret.clone(),
        token_duration_hours: config.token_duration_hours,
    };

    let state = AppState {
        session_manager,
        auth_config,
        llm_router: Arc::new(RwLock::new(llm_router)),
        default_model: config.default_model.clone(),
        cron_scheduler,
        memory_manager,
        skill_manager,
        tool_registry,
        user_store,
        log_writer,
        feishu_config_store: Arc::new(FeishuConfigStore::new(pool)),
        feishu_client_cache: Arc::new(dashmap::DashMap::new()),
        data_dir: data_dir.clone(),
    };

    // ── 14. Build router ──
    let app = router::build_router(state).layer(CorsLayer::permissive());

    // ── 15. Start Axum server on localhost:3000 in background ──
    let addr = "127.0.0.1:3000";
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            error!(error = %e, addr = %addr, "Failed to bind server port");
            show_error_and_exit(&format!("无法启动服务器 (端口 {} 可能被占用):\n\n{}", addr, e));
            return;
        }
    };
    info!(%addr, "Jcowork Desktop server listening");

    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::error!(error = %e, "Server error");
        }
    });

    // ── 16. Wait for server to be ready ──
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    info!("Server ready, launching Tauri window");

    // ── 17. Launch Tauri desktop app ─
    if let Err(e) = tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![open_in_browser])
        .run(tauri::generate_context!()) {
        error!(error = %e, "Tauri application error");
        eprintln!("Tauri error: {}", e);
    }

    info!("Jcowork Desktop stopped");
}

/// Open a URL in the system's default browser.
#[tauri::command]
fn open_in_browser(url: String) -> Result<(), String> {
    open::that(&url).map_err(|e| format!("Failed to open browser: {}", e))
}

/// Show a native error dialog and exit.
fn show_error_and_exit(msg: &str) {
    eprintln!("\n{}", msg);
    // Try to show a native dialog via osascript (macOS)
    let _ = std::process::Command::new("osascript")
        .args([
            "-e",
            &format!(
                "display dialog \"{}\" buttons {{\"OK\"}} default button \"OK\" with icon stop with title \"Jcowork 启动失败\"",
                msg.replace('"', "\\\"").replace('\n', "\\n")
            ),
        ])
        .output();
    std::process::exit(1);
}
