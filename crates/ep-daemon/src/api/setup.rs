//! 首次运行初始化状态探测（T-40，计划2 M1 首次运行向导）。
//!
//! `GET /api/setup/status` — 只读端点：WebUI 首访时判断是否弹出首次运行
//! 向导（onboarding），并向导各步提供预填数据（root / workspace / port /
//! language）。
//!
//! 「已初始化」的唯一判据 = `config/app.toml` **存在**（不引入哨兵文件，
//! 用「配置文件是否存在」这一现有事实）：
//! - 发行包只带 `app.toml.example` 模板，daemon 启动亦不物化缺省文件
//!   （main.rs 走 [`AppConfig::load`]：文件缺失时用内存缺省值、不落盘）；
//! - 向导「完成」与「跳过」都经 PUT /api/config 持久化 → 文件出现 →
//!   探测转为已初始化，此后（含重启、换浏览器）不再弹向导；
//! - 旧环境 / 升级环境 / deploy.sh 部署（部署向导已合并写入）文件本就
//!   存在 → 恒返回已初始化，零打扰。
//!
//! 前端门控策略：`initialized = false` 才弹向导；探测请求失败（服务暂
//! 不可达）按已初始化处理——直进主界面（零强制，向导不承载业务逻辑）。

use std::sync::Arc;

use axum::{extract::State, routing::get, Json, Router};
use serde::Serialize;

use crate::state::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/setup/status", get(get_setup_status))
}

/// GET /api/setup/status 响应（T-40）。
#[derive(Debug, Serialize)]
pub struct SetupStatus {
    /// `config/app.toml` 是否已存在（true = 已初始化，WebUI 不弹向导）
    pub initialized: bool,
    /// 应用根目录（daemon 启动期 resolve_root 解析结果，绝对路径展示用）
    pub root: String,
    /// 主配置文件预期路径（`<root>/config/app.toml`）
    pub config_file: String,
    /// `pipeline.workspace_dir` 解析后的绝对路径（resolve(root) 只读视图）
    pub workspace_dir: String,
    /// `pipeline.workspace_dir` 配置原始串（相对保持相对；向导输入框预填/回写）
    pub workspace_dir_raw: String,
    /// 当前 `[server].port`（向导端口步预填）
    pub port: u16,
    /// 当前 `[general].language`（向导语言步预填）
    pub language: String,
}

/// GET /api/setup/status — 只读探测（T-40）。
///
/// 全部字段来自内存配置快照与一次文件存在性 stat；唯一的磁盘访问
/// （`is_file`）按 spawn_blocking 纪律挪到阻塞线程池，不占用 reactor。
async fn get_setup_status(State(state): State<Arc<AppState>>) -> Json<SetupStatus> {
    // 文件名与 ep-core CONFIG_FILE_NAME 保持一致（crate 私有常量，此处
    // 以字面量 + 测试钉住；main.rs 测试同样按字面量 join 的先例）
    let config_file = state.root.join("config").join("app.toml");
    let initialized = {
        let path = config_file.clone();
        tokio::task::spawn_blocking(move || path.is_file())
            .await
            .unwrap_or(false)
    };
    let (workspace_dir, workspace_dir_raw, port, language) = {
        let config = state.config.read().await;
        (
            config
                .resolve_workspace_dir(&state.root)
                .to_string_lossy()
                .into_owned(),
            config.pipeline.workspace_dir.clone(),
            config.server.port,
            config.general.language.clone(),
        )
    };
    Json(SetupStatus {
        initialized,
        root: state.root.to_string_lossy().into_owned(),
        config_file: config_file.to_string_lossy().into_owned(),
        workspace_dir,
        workspace_dir_raw,
        port,
        language,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ep_core::config::AppConfig;
    use http_body_util::BodyExt;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tower::ServiceExt;

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    /// 唯一 tempdir AppState（与 api/config.rs 测试同一套路）
    fn seq_state() -> Arc<AppState> {
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "ep-api-setup-{seq}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        Arc::new(AppState::new(
            root,
            AppConfig::default(),
            vec![],
            vec![],
            ep_core::port::PortManager::new(18000, 19000),
        ))
    }

    // T-40：全新环境（无 app.toml）→ initialized=false + root/workspace 绝对路径回显
    #[tokio::test]
    async fn fresh_root_reports_uninitialized_with_resolved_paths() {
        let state = seq_state();
        let status = get_setup_status(State(state.clone())).await.0;

        assert!(!status.initialized, "无 app.toml 应视为未初始化");
        assert_eq!(
            status.root,
            state.root.to_string_lossy().to_string(),
            "root 应为 daemon 解析的应用根目录"
        );
        assert_eq!(
            status.config_file,
            state.root.join("config").join("app.toml").to_string_lossy().to_string()
        );
        // 缺省 workspace_dir = "workspace"（相对）→ 基于 root 解析为绝对路径
        assert_eq!(
            status.workspace_dir,
            state.root.join("workspace").to_string_lossy().to_string()
        );
        // 原始串保持缺省相对形态（#48 解析与持久化分离口径）
        assert_eq!(status.workspace_dir_raw, "workspace");
        assert_eq!(status.port, 9800, "缺省 [server].port");
        assert_eq!(status.language, "zh-CN", "缺省 [general].language");

        let _ = std::fs::remove_dir_all(state.root.as_path());
    }

    // T-40：app.toml 存在 → initialized=true（完成/跳过向导、升级环境、
    // deploy.sh 部署环境共用的「已初始化」唯一判据）
    #[tokio::test]
    async fn existing_config_file_reports_initialized() {
        let state = seq_state();
        // 模拟向导落盘：save() 会创建 config/ 目录并原子写盘
        AppConfig::default()
            .save(state.root.join("config").as_path())
            .expect("save default config");

        let status = get_setup_status(State(state.clone())).await.0;
        assert!(status.initialized, "app.toml 存在即视为已初始化");

        let _ = std::fs::remove_dir_all(state.root.as_path());
    }

    // 路由注册：GET /setup/status 命中（经 router() 走完整 axum 栈）
    #[tokio::test]
    async fn route_registered_and_shape_stable() {
        let state = seq_state();
        let app = router().with_state(state.clone());
        let resp = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/setup/status")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);

        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        // 响应形状钉死（前端 SetupStatus 类型依赖）：六字段齐全
        for key in [
            "initialized",
            "root",
            "config_file",
            "workspace_dir",
            "workspace_dir_raw",
            "port",
            "language",
        ] {
            assert!(json.get(key).is_some(), "响应缺少字段 {key}");
        }
        assert_eq!(json["initialized"], false);

        let _ = std::fs::remove_dir_all(state.root.as_path());
    }
}
