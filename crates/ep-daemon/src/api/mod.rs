pub mod config;
pub mod deps;
pub mod devices;
pub mod execute;
pub mod health;
pub mod models;
pub mod modules;
pub mod pipelines;
pub mod tasks;
pub mod upload;

use std::sync::Arc;

use axum::http::StatusCode;
use axum::{Json, Router};
use serde_json::{Value, json};

use crate::state::AppState;

/// Build the full `/api/*` route tree.
///
/// Wave 2 骨架路由已预注册（tasks / upload / execute / models 下载删除等），
/// 对应 stub handler 统一返回 501 + {"error":"功能即将上线"}，
/// 各文件头部注释标明接管代理。
pub fn api_router() -> Router<Arc<AppState>> {
    Router::new()
        .merge(health::router())
        .merge(devices::router())
        .merge(modules::router())
        .merge(config::router())
        .merge(pipelines::router())
        .merge(execute::router())
        .merge(models::router())
        .merge(upload::router())
        .merge(tasks::router())
        .merge(deps::router())
        // 未匹配的 /api/* → 404 + JSON，避免落入 SPA 的 HTML fallback
        .fallback(api_not_found)
}

/// /api/* 下未匹配路由的统一响应
async fn api_not_found() -> (StatusCode, Json<Value>) {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "error": "接口不存在" })),
    )
}
