// Grouped imports: std first, then external crates, then local modules
use std::net::SocketAddr;
use std::sync::Arc;

use tracing::info;

mod error;
mod handlers;
mod middleware;
mod models;
mod services;
mod utils;

use handlers::health_handler;
use models::AppState;

/// Application entry point.
///
/// Demonstrates:
/// - Grouped imports (std, external, local)
/// - Module declarations
/// - tracing for logging
/// - async main
#[tokio::main]
async fn main() {
    tracing_subscriber_init();

    let state = Arc::new(AppState::default());
    let addr = SocketAddr::from(([127, 0, 0, 1], 3000));

    info!(%addr, "Starting server");

    if let Err(e) = run_server(state, addr).await {
        tracing::error!(error = %e, "Server failed");
    }
}

fn tracing_subscriber_init() {
    // Placeholder for tracing subscriber setup
}

async fn run_server(
    _state: Arc<AppState>,
    _addr: SocketAddr,
) -> Result<(), Box<dyn std::error::Error>> {
    health_handler();
    Ok(())
}
