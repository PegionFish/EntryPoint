use std::sync::Arc;

use axum::{
    Router,
    extract::State,
    http::StatusCode,
    routing::{get, put},
    Json,
};
use serde_json::{Value, json};

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

/// PUT /api/config — 整体替换内存配置并持久化到 config/app.toml。
///
/// 持有 config 写锁期间完成替换 + save()，避免并发读写不一致。
/// 落盘失败返回 500 + 中文错误（内存配置已替换，但不持久化成功前重启会丢失）。
pub async fn put_config(
    State(state): State<Arc<AppState>>,
    Json(new_config): Json<AppConfig>,
) -> Result<Json<AppConfig>, (StatusCode, Json<Value>)> {
    let config_dir = state.root.join("config");
    let mut config = state.config.write().await;
    *config = new_config;
    if let Err(e) = config.save(&config_dir) {
        tracing::error!(error = %e, "failed to persist config");
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("保存配置失败：{e}") })),
        ));
    }
    Ok(Json(config.clone()))
}
