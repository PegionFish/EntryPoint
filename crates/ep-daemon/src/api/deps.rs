//! GET /api/deps — 外部依赖检测报告

use std::sync::Arc;

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};

use ep_core::deps;

use crate::state::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/deps", get(check_deps))
}

async fn check_deps(State(state): State<Arc<AppState>>) -> Json<deps::DepReport> {
    let modules = state.modules.read().await;
    let ids: Vec<&str> = modules
        .iter()
        .filter_map(|m| m.manifest.as_ref().map(|mf| mf.module.id.as_str()))
        .collect();
    let report = deps::check_all_deps(&state.root, &ids);
    Json(report)
}
