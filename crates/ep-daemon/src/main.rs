mod api;
mod state;
mod ws;

use std::net::SocketAddr;
use axum::Router;
use tower_http::cors::{CorsLayer, Any};
use tower_http::services::ServeDir;
use tracing_subscriber::EnvFilter;

use crate::state::AppState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "ep_daemon=info,ep_core=info".into()),
        )
        .init();

    tracing::info!("EntryPoint Daemon starting...");

    let state = AppState::new();

    let _state = state; // Will be wired in Wave 2

    let app = Router::new()
        .merge(api::api_router())
        .merge(ws::ws_router())
        .fallback_service(ServeDir::new("crates/ep-webui/static"))
        .layer(CorsLayer::new()
            .allow_origin(Any)
            .allow_methods(Any)
            .allow_headers(Any));

    let addr = SocketAddr::from(([0, 0, 0, 0], 9800));
    tracing::info!("Listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app.into_make_service())
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    tracing::info!("Daemon shut down gracefully");
    Ok(())
}

async fn shutdown_signal() {
    tokio::signal::ctrl_c()
        .await
        .expect("failed to install CTRL+C signal handler");
}
