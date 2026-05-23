//! Jcowork Report Search Service
//!
//! A standalone HTTP service (port 3001) that:
//!  1. Watches ~/.jcowork/data/reports/ for new PDF files
//!  2. Parses them with pdftext and splits into chunks
//!  3. Indexes chunks in a local SQLite FTS5 database
//!  4. Exposes a REST search API for use by the report_search tool

mod api;
mod db;
mod parser;
mod watcher;

use anyhow::Result;
use std::sync::Arc;
use tower_http::cors::CorsLayer;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "jcowork_report_search=info,tower_http=warn".parse().unwrap()),
        )
        .init();

    // Configuration from environment with defaults
    let data_dir = shellexpand::tilde(
        &std::env::var("JCWORK_DATA_DIR").unwrap_or_else(|_| "~/.jcowork/data".to_string()),
    )
    .to_string();
    let reports_dir = std::env::var("JCWORK_REPORTS_DIR")
        .unwrap_or_else(|_| format!("{}/reports", data_dir));
    let db_path = std::env::var("JCWORK_REPORT_INDEX_DB")
        .unwrap_or_else(|_| format!("{}/reports_index.db", data_dir));
    let port: u16 = std::env::var("JCWORK_REPORT_SEARCH_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(3001);
    let watch_interval: u64 = std::env::var("JCWORK_REPORT_WATCH_INTERVAL")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(30);

    info!(data_dir = %data_dir, reports_dir = %reports_dir, db = %db_path, port = port);

    // Ensure directories exist
    tokio::fs::create_dir_all(&data_dir).await?;
    tokio::fs::create_dir_all(&reports_dir).await?;

    // Initialize database
    let pool = Arc::new(db::init_pool(&db_path).await?);
    info!("Report search database initialized at {}", db_path);

    // Start background file watcher
    let watcher_pool = pool.clone();
    let watcher_dir = reports_dir.clone();
    tokio::spawn(async move {
        watcher::run_watcher(watcher_pool, watcher_dir, watch_interval).await;
    });

    // Build HTTP server
    let app_state = api::AppState {
        pool,
        reports_dir,
    };
    let app = api::build_router(app_state).layer(CorsLayer::permissive());

    let addr = format!("0.0.0.0:{}", port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    info!("Report Search Service listening on {}", addr);

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

async fn shutdown_signal() {
    tokio::signal::ctrl_c()
        .await
        .expect("Failed to install Ctrl+C handler");
    info!("Shutdown signal received");
}
