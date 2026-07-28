pub mod config;
pub mod devices;
pub mod health;
pub mod models;
pub mod modules;
pub mod pipelines;

use std::sync::Arc;

use axum::Router;

use crate::state::AppState;

/// Build the full `/api/*` route tree.
pub fn api_router() -> Router<Arc<AppState>> {
    Router::new()
        .merge(health::router())
        .merge(devices::router())
        .merge(modules::router())
        .merge(config::router())
        .merge(pipelines::router())
        .merge(models::router())
}
