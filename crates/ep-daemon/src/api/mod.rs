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

use axum::extract::State;
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

/// /api/* 下未匹配路由的统一响应（i18n：apiCore.apiNotFound）
async fn api_not_found(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    err_response(&state, StatusCode::NOT_FOUND, "apiCore.apiNotFound", &[]).await
}

/// i18n 错误响应：`{"error": t(lang, key, params)}`，`lang` 取自
/// `config.general.language`（经 [`ep_core::i18n::normalize_language`] 归一化）。
///
/// Wave 1 代理将各端点从文件内私有的 `error()` / `error_response()` 逐步迁移到
/// 本函数（旧辅助函数在迁移完成前保留，勿删）。键格式与插值规则见
/// `ep_core::i18n` 模块文档；键缺失时返回键本身。
///
/// `params` 值类型为 `String`：调用方先 `format!(…)` 再传入。
#[allow(dead_code)] // Wave 1 迁移期预置，各 API 文件陆续接管消费
pub async fn err_response(
    state: &Arc<AppState>,
    status: StatusCode,
    key: &str,
    params: &[(&str, String)],
) -> (StatusCode, Json<Value>) {
    let lang = state.lang().await;
    let params: Vec<(&str, &str)> = params.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let message = ep_core::i18n::t(&lang, key, &params);
    (status, Json(json!({ "error": message })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    fn test_state(language: &str) -> Arc<AppState> {
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        let mut config = ep_core::config::AppConfig::default();
        config.general.language = language.to_string();
        Arc::new(AppState::new(
            std::env::temp_dir().join(format!("ep-api-mod-test-{}-{seq}", std::process::id())),
            config,
            vec![],
            vec![],
            ep_core::port::PortManager::new(18000, 19000),
        ))
    }

    // 默认语言 zh-CN：中文文案（旧 error_response 行为不受影响）
    #[tokio::test]
    async fn err_response_zh_cn() {
        let state = test_state("zh-CN");
        let (status, body) =
            err_response(&state, StatusCode::NOT_FOUND, "common.action.cancel", &[]).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body.0["error"], "取消");
    }

    // en 配置 → 英文文案
    #[tokio::test]
    async fn err_response_en() {
        let state = test_state("en");
        let (_, body) =
            err_response(&state, StatusCode::BAD_REQUEST, "common.action.cancel", &[]).await;
        assert_eq!(body.0["error"], "Cancel");
    }

    // {{name}} 插值（params 值为 String）
    #[tokio::test]
    async fn err_response_interpolates_params() {
        let state = test_state("zh-CN");
        let (_, body) = err_response(
            &state,
            StatusCode::CONFLICT,
            "common.tip.confirmDeleteNamed",
            &[("name", "large-v3".to_string())],
        )
        .await;
        assert_eq!(body.0["error"], "确认删除 large-v3？此操作不可撤销");
    }

    // 键缺失 → 返回键本身（Wave 1 填充命名空间前的安全兜底）
    #[tokio::test]
    async fn err_response_missing_key_falls_back_to_key() {
        let state = test_state("zh-CN");
        let (status, body) = err_response(
            &state,
            StatusCode::INTERNAL_SERVER_ERROR,
            "apiCore.notThereYet",
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body.0["error"], "apiCore.notThereYet");
    }
}
