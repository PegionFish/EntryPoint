//! 全局队列控制 API（T-15）：暂停/恢复派发 + 队列运行态快照。
//!
//! 队列账本与暂停标志由 [`crate::api::execute::execution`] 的进程级调度器
//! 持有，本文件只做薄路由层：
//!
//! - `GET  /api/queue`         → `{paused, running, queued}`
//! - `POST /api/queue/pause`   → 暂停派发（运行中任务照常完成，排队任务保留）
//! - `POST /api/queue/resume`  → 恢复派发并立即尝试准入
//!
//! 暂停语义见 [`execution::pause_queue`]：只挡新任务准入，不打断运行中任务。

use std::sync::Arc;

use axum::{
    extract::State,
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};

use crate::api::execute::execution::{self, QueueStatus};
use crate::state::AppState;

/// `/api/queue/*` 路由树（路径不含 `/api` 前缀：由 main.rs `nest("/api")` 统一挂载；
/// 此前误写完整前缀导致实际落在 `/api/api/queue`，前端恒 404——2026-11-01 修复）
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/queue", get(queue_status))
        .route("/queue/pause", post(pause_queue))
        .route("/queue/resume", post(resume_queue))
}

/// 统一响应体：`{paused, running, queued}`
fn status_body(status: QueueStatus) -> Json<Value> {
    Json(json!({
        "paused": status.paused,
        "running": status.running,
        "queued": status.queued,
    }))
}

/// 读取队列运行态（前端队列状态条轮询消费）
async fn queue_status() -> (StatusCode, Json<Value>) {
    (StatusCode::OK, status_body(execution::queue_status()))
}

/// 暂停派发；返回暂停后的队列快照
async fn pause_queue() -> (StatusCode, Json<Value>) {
    (StatusCode::OK, status_body(execution::pause_queue()))
}

/// 恢复派发；返回恢复后的队列快照
async fn resume_queue(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    (StatusCode::OK, status_body(execution::resume_queue(&state)))
}
