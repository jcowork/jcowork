//! Jcowork Desktop — Tauri v2 桌面应用
//!
//! 启动流程:
//! 1. 初始化 tracing + 加载 .env
//! 2. 复用 jcowork-server 的初始化逻辑创建 AppState + Axum app
//! 3. 在后台 tokio task 中启动 Axum 服务器 (localhost:3000)
//! 4. 等待服务器就绪后，通过 Tauri 打开窗口加载 http://localhost:3000

use anyhow::Result;
use std::sync::Arc;
use tower_http::cors::CorsLayer;
use tracing::{info, warn};

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

#[tokio::main]
async fn main() -> Result<()> {
    // Load .env file
    let _ = dotenvy::dotenv();

    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "jcowork=info".parse().unwrap()),
        )
        .init();

    // Load configuration
    let config = ServerConfig::from_env();
    info!(
        ?config.host,
        ?config.port,
        ?config.data_dir,
        ?config.default_model,
        "Starting Jcowork Desktop"
    );

    // Create data directory
    let data_dir = shellexpand::tilde(&config.data_dir).to_string();
    tokio::fs::create_dir_all(&data_dir).await?;

    // Initialize LLM router from environment
    let llm_router = jcowork_llm::LlmRouter::from_env()?;
    info!(
        providers = ?llm_router.available_providers(),
        "LLM providers registered"
    );

    // Initialize memory provider (SQLite)
    let db_path = format!("{}/jcowork.db", data_dir);
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(10)
        .connect(&format!("sqlite:{}?mode=rwc", db_path))
        .await?;
    jcowork_storage::migration::run_migrations(&pool).await?;
    let memory_provider = BuiltinMemoryProvider::new(pool.clone());
    memory_provider.init().await?;
    info!(db = %db_path, "Memory database initialized");

    let mut memory_manager = MemoryManager::new();
    memory_manager.add_provider(Arc::new(memory_provider));
    let memory_manager = Arc::new(memory_manager);

    // Initialize skill manager
    let skill_manager = Arc::new(SkillManager::new(pool.clone()));
    info!("Skill manager initialized");

    // Initialize cron scheduler
    let cron_scheduler = Arc::new(jcowork_cron::CronScheduler::new());

    // Initialize user store
    let user_store = Arc::new(jcowork_storage::UserStore::new(&data_dir).await?);
    info!("User store initialized");

    // Initialize log writer
    let log_dir = format!("{}/logs", data_dir);
    let log_writer = Arc::new(LogWriter::new(log_dir.into()).await?);
    info!("Log writer initialized");

    // Initialize tool registry
    let tool_registry = jcowork_gateway::ws::build_tool_registry(
        cron_scheduler.clone(),
        memory_manager.clone(),
        log_writer.clone(),
    );

    // Initialize session manager
    let session_manager = Arc::new(SessionManager::new());

    // Build app state
    let auth_config = AuthConfig {
        jwt_secret: config.jwt_secret.clone(),
        token_duration_hours: config.token_duration_hours,
    };

    let state = AppState {
        session_manager,
        auth_config,
        llm_router: Arc::new(llm_router),
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

    // Build router
    let app = router::build_router(state).layer(CorsLayer::permissive());

    // Spawn the report-search sidecar service
    spawn_report_search_sidecar(&data_dir);

    // Start Axum server on localhost:3000 in background
    let addr = "127.0.0.1:3000";
    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!(%addr, "Jcowork Desktop server listening");

    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::error!(error = %e, "Server error");
        }
    });

    // Wait for server to be ready
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    info!("Server ready, launching Tauri window");

    // Launch Tauri desktop app
    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("error while running tauri application");

    info!("Jcowork Desktop stopped");
    Ok(())
}

/// Spawn the jcowork-report-search binary as a background sidecar.
fn spawn_report_search_sidecar(data_dir: &str) {
    let current_exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            warn!("Could not determine current exe path: {}", e);
            return;
        }
    };
    let sidecar = current_exe
        .parent()
        .map(|dir| dir.join("jcowork-report-search"))
        .unwrap_or_else(|| std::path::PathBuf::from("jcowork-report-search"));

    if !sidecar.exists() {
        warn!(
            sidecar = %sidecar.display(),
            "jcowork-report-search binary not found — report search disabled"
        );
        return;
    }

    let data_dir = data_dir.to_string();
    tokio::spawn(async move {
        info!(bin = %sidecar.display(), "Starting jcowork-report-search sidecar");
        let status = tokio::process::Command::new(&sidecar)
            .env("JCWORK_DATA_DIR", &data_dir)
            .status()
            .await;
        match status {
            Ok(s) => warn!("jcowork-report-search exited with: {}", s),
            Err(e) => warn!("jcowork-report-search failed to start: {}", e),
        }
    });
}
