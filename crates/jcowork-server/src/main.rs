//! Jcowork Server - Main entry point.
//!
//! Starts the axum HTTP server with WebSocket support,
//! initializes the session manager, tool registry, and LLM router.
//! Also spawns the jcowork-report-search sidecar service (port 3001).

use anyhow::Result;
use std::sync::Arc;
use std::sync::RwLock;
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
    info!(?config.host, ?config.port, ?config.data_dir, ?config.default_model, "Starting Jcowork Server");

    // Create data directory
    let data_dir = shellexpand::tilde(&config.data_dir).to_string();
    tokio::fs::create_dir_all(&data_dir).await?;

    // Initialize LLM router from environment
    let llm_router = jcowork_llm::LlmRouter::from_env()?;
    info!(providers = ?llm_router.available_providers(), "LLM providers registered");

    // Migrate providers to persistent file on first run.
    // This loads from env vars + bundled providers.json and saves to ~/.jcowork/data/providers.json
    // so the UI can manage them. On subsequent runs, the router loads from this file.
    let providers_file = jcowork_llm::LlmRouter::providers_file_path(&data_dir);
    if !std::path::Path::new(&providers_file).exists() {
        // Load bundled providers.json (from project root or Tauri resources)
        let bundled_configs = jcowork_llm::LlmRouter::load_provider_configs_from_path("providers.json")
            .or_else(|_| {
                // Try Tauri resource path
                let exe = std::env::current_exe().ok();
                let res_dir = exe.as_ref().and_then(|p| p.parent()).and_then(|p| p.parent()).map(|p| p.join("Resources"));
                if let Some(ref dir) = res_dir {
                    let candidate = dir.join("providers.json");
                    if candidate.exists() {
                        return jcowork_llm::LlmRouter::load_provider_configs_from_path(candidate.to_str().unwrap_or(""));
                    }
                }
                Err(anyhow::anyhow!("No bundled providers.json found"))
            })
            .unwrap_or_default();

        // Convert ProviderConfig to ProviderEntry using env vars for API keys
        let entries: Vec<jcowork_llm::ProviderEntry> = bundled_configs.into_iter().map(|c| {
            let api_key = if c.env_key.is_empty() {
                String::new()
            } else {
                std::env::var(&c.env_key).unwrap_or_default()
            };
            // Check for base URL override from env
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
                tracing::warn!(err = %e, "Failed to save initial providers to file");
            } else {
                info!(path = %providers_file, count = entries.len(), "Migrated providers to persistent file");
            }
        }
    }

    // Load router from persistent file (if it exists), otherwise use env-based router
    let llm_router = if std::path::Path::new(&providers_file).exists() {
        match jcowork_llm::LlmRouter::load_entries_from_file(&providers_file) {
            Ok(entries) => {
                let router = jcowork_llm::LlmRouter::rebuild_from_entries(&entries);
                info!(providers = ?router.available_providers(), "LLM router loaded from persistent file");
                router
            }
            Err(e) => {
                tracing::warn!(err = %e, "Failed to load from providers file, using env-based router");
                llm_router
            }
        }
    } else {
        llm_router
    };

    // Initialize memory provider (SQLite)
    let db_path = format!("{}/jcowork.db", data_dir);
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(10)
        .connect(&format!("sqlite:{}?mode=rwc", db_path))
        .await?;
    // Run schema migrations (tables: memories, skills, cron_jobs, etc.)
    jcowork_storage::migration::run_migrations(&pool).await?;
    let memory_provider = BuiltinMemoryProvider::new(pool.clone());
    memory_provider.init().await?;
    info!(db = %db_path, "Memory database initialized");

    let mut memory_manager = MemoryManager::new();
    memory_manager.add_provider(std::sync::Arc::new(memory_provider));
    let memory_manager = std::sync::Arc::new(memory_manager);

    // Initialize skill manager (shares the same pool — no locking conflicts)
    let skill_manager = Arc::new(SkillManager::new(pool.clone()));
    info!("Skill manager initialized");

    // Initialize cron scheduler
    let cron_scheduler = Arc::new(jcowork_cron::CronScheduler::new());

    // Initialize user store (global accounts database)
    let user_store = Arc::new(jcowork_storage::UserStore::new(&data_dir).await?);
    info!("User store initialized");

    // Initialize log writer
    let log_dir = format!("{}/logs", data_dir);
    let log_writer = Arc::new(LogWriter::new(log_dir.into()).await?);
    info!("Log writer initialized");

    // Initialize tool registry
    let tool_registry = jcowork_gateway::ws::build_tool_registry(cron_scheduler.clone(), memory_manager.clone(), log_writer.clone());

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

    // Build router
    let app = router::build_router(state)
        .layer(CorsLayer::permissive());

    // Spawn the report-search sidecar service
    spawn_report_search_sidecar(&data_dir);

    // Bind and serve
    let addr = format!("{}:{}", config.host, config.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    info!(%addr, "Jcowork Server listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    info!("Jcowork Server stopped");
    Ok(())
}

async fn shutdown_signal() {
    tokio::signal::ctrl_c()
        .await
        .expect("Failed to install Ctrl+C handler");
    info!("Shutdown signal received");
}

/// Spawn the jcowork-report-search binary as a background sidecar.
/// It listens on port 3001 and indexes PDFs in the data_dir/reports directory.
fn spawn_report_search_sidecar(data_dir: &str) {
    // Locate the binary next to the current executable
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
            "jcowork-report-search binary not found — report search disabled. \
             Build it with: cargo build --bin jcowork-report-search"
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
