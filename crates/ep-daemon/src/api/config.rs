use std::sync::Arc;

use axum::{
    Router,
    extract::State,
    routing::{get, put},
    Json,
};

use ep_core::config::AppConfig;

use crate::state::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/config", get(get_config))
        .route("/config", put(put_config))
}

pub async fn get_config(
    State(state): State<Arc<AppState>>,
) -> Json<AppConfig> {
    let config = state.config.read().await;
    Json(config.clone())
}

pub async fn put_config(
    State(state): State<Arc<AppState>>,
    Json(new_config): Json<AppConfig>,
) -> Json<AppConfig> {
    let mut config = state.config.write().await;
    *config = new_config;
    Json(config.clone())
}
