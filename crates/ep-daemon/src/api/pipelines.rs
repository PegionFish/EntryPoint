use std::sync::Arc;

use axum::{
    Router,
    routing::{get, post},
    Json,
};
use serde_json::{Value, json};

use crate::state::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/pipelines", get(list_pipelines))
        .route("/pipelines/execute", post(execute_pipeline))
        .route("/pipelines/{id}/status", get(pipeline_status))
}

/// Placeholder — returns an empty list.
async fn list_pipelines() -> Json<Value> {
    Json(json!([]))
}

/// Placeholder — pipeline execution is not yet implemented.
async fn execute_pipeline() -> Json<Value> {
    Json(json!({
        "error": "pipeline execution not yet implemented"
    }))
}

/// Placeholder — returns unknown status.
async fn pipeline_status() -> Json<Value> {
    Json(json!({
        "status": "unknown",
        "message": "pipeline execution not yet implemented"
    }))
}
