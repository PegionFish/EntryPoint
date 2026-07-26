use std::sync::Arc;

use axum::{Json, Router, routing::get};
use serde_json::{Value, json};

use crate::state::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/health", get(health_check))
}

pub async fn health_check() -> Json<Value> {
    Json(json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION")
    }))
}
