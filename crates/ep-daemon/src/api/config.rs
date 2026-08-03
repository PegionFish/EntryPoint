use std::sync::Arc;

use axum::{
    Router,
    extract::State,
    http::StatusCode,
    routing::{get, put},
    Json,
};
use serde_json::Value;

use ep_core::config::AppConfig;

use super::err_response;
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
/// 落盘失败返回 500 + i18n 错误（apiCore.config.saveFailed，文案语言随
/// config.general.language；内存配置已替换，但持久化成功前重启会丢失）。
///
/// 注意：错误分支必须先释放写锁再调 [`err_response`] —— 其内部经
/// `state.lang()` 读 config，持写锁调用会在 tokio RwLock 上死锁。
pub async fn put_config(
    State(state): State<Arc<AppState>>,
    Json(new_config): Json<AppConfig>,
) -> Result<Json<AppConfig>, (StatusCode, Json<Value>)> {
    let config_dir = state.root.join("config");
    let (save_result, snapshot) = {
        let mut config = state.config.write().await;
        *config = new_config;
        let result = config.save(&config_dir);
        let snapshot = config.clone();
        (result, snapshot)
    };
    if let Err(e) = save_result {
        tracing::error!(error = %e, "failed to persist config");
        return Err(err_response(
            &state,
            StatusCode::INTERNAL_SERVER_ERROR,
            "apiCore.config.saveFailed",
            &[("detail", e.to_string())],
        )
        .await);
    }
    Ok(Json(snapshot))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    /// 构造指定语言的测试 AppState（root 指向调用方给定路径）
    fn test_state(root: std::path::PathBuf, language: &str) -> Arc<AppState> {
        let mut config = AppConfig::default();
        config.general.language = language.to_string();
        Arc::new(AppState::new(
            root,
            config,
            vec![],
            vec![],
            ep_core::port::PortManager::new(18000, 19000),
        ))
    }

    // 落盘失败 + config.language=en → 500 + 英文错误（detail 为 ep-core 英文技术细节）
    #[tokio::test]
    async fn put_config_save_failure_en() {
        // root 指向一个普通文件 → root/config 无法创建 → save 失败
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        let file_root = std::env::temp_dir().join(format!(
            "ep-api-config-file-root-{}-{seq}",
            std::process::id()
        ));
        std::fs::write(&file_root, "blocker").unwrap();
        let state = test_state(file_root.clone(), "en");

        // put_config 会先用请求体整体替换配置，错误语言取自替换后的配置，
        // 因此请求体同样携带 en
        let mut new_config = AppConfig::default();
        new_config.general.language = "en".to_string();

        let result = put_config(State(state), Json(new_config)).await;
        let (status, body) = result.expect_err("save should fail when root is a file");
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        let error = body.0["error"].as_str().unwrap();
        assert!(
            error.starts_with("Failed to save configuration"),
            "expected English error, got: {error}"
        );

        let _ = std::fs::remove_file(&file_root);
    }

    // 默认语言 zh-CN：落盘失败文案与迁移前一致（保存配置失败：…）
    #[tokio::test]
    async fn put_config_save_failure_zh_cn() {
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        let file_root = std::env::temp_dir().join(format!(
            "ep-api-config-file-root-zh-{}-{seq}",
            std::process::id()
        ));
        std::fs::write(&file_root, "blocker").unwrap();
        let state = test_state(file_root.clone(), "zh-CN");

        let result = put_config(State(state), Json(AppConfig::default())).await;
        let (status, body) = result.expect_err("save should fail when root is a file");
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        let error = body.0["error"].as_str().unwrap();
        assert!(
            error.starts_with("保存配置失败"),
            "expected zh-CN error, got: {error}"
        );

        let _ = std::fs::remove_file(&file_root);
    }
}
