//! 配置 API：GET 全量读取 / PUT 深度合并（§8.2，P1-9）。
//!
//! PUT 语义（Wave 3 C7 落地，config 层支持为 A1 的 [`AppConfig::merge_partial`]）：
//! - 请求体为 **JSON patch**（可只含个别字段/分区），与当前配置深度合并；
//!   缺省字段保留原值，显式字段覆盖，`active_models` 按键合并（仲裁 #7：未知键忽略）
//! - 合并成功 → 落盘 config/app.toml → 内存态（`state.config`，全体读者共享真源）更新
//! - 响应为合并后的完整配置 + `requires_restart`（重启敏感项是否被改动，§8.2）
//! - patch 非法（JSON 语法错误 / 非对象 / 字段类型不匹配）→ 400，内存配置保持不变
//! - 落盘失败 → 500（内存已合并，持久化成功前重启会丢失，与迁移前语义一致）

use std::sync::Arc;

use axum::{
    Router,
    extract::State,
    http::StatusCode,
    routing::{get, put},
    Json,
};
use serde::Serialize;
use serde_json::Value;

use ep_core::config::AppConfig;

use super::err_response;
use crate::state::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/config", get(get_config))
        // 路由层以 Value 接收任意 JSON patch；handler 泛型保留
        // `Json<AppConfig>`（全量配置）直调兼容（main.rs 既有测试）。
        .route("/config", put(put_config::<Value>))
}

pub async fn get_config(State(state): State<Arc<AppState>>) -> Json<AppConfig> {
    let config = state.config.read().await;
    Json(config.clone())
}

/// PUT /api/config 响应（§8.2）：合并后的完整配置 + `requires_restart`。
///
/// `#[serde(flatten)]` 使 JSON 形状为「AppConfig 全字段与 requires_restart 平级」，
/// 与"直接读取 AppConfig"的旧客户端向后兼容（多出的键被忽略）。
/// `Deref` 到 [`AppConfig`] 使既有直调 handler 的调用点（`resp.0.general.…`
/// 风格字段访问）保持源码兼容。
#[derive(Debug, Serialize)]
pub struct PutConfigResponse {
    #[serde(flatten)]
    pub config: AppConfig,
    /// 本次改动是否触及重启敏感项（见 [`restart_sensitive_changed`]，§8.2）
    pub requires_restart: bool,
}

impl std::ops::Deref for PutConfigResponse {
    type Target = AppConfig;

    fn deref(&self) -> &Self::Target {
        &self.config
    }
}

/// 重启敏感项判定（§8.2；逐项依据见 C7 交付报告）：
///
/// | 字段 | 只在启动期消费的位置 |
/// |---|---|
/// | `server.host` / `server.port` | main.rs 启动时绑定监听地址 |
/// | `ports.range_start` / `ports.range_end` | PortManager 构造期固定区间 |
/// | `pipeline.workspace_dir` | 启动期 resolve_paths + 任务产物 ServeDir 根启动期固定（tasks.rs） |
/// | `compute.refresh_interval_secs` | 设备轮询任务启动期取间隔（不热跟随） |
/// | `compute.cuda_libs_dir` | AppState 构造期注入 ProcessManager |
/// | `general.log_level` | tracing subscriber 启动期初始化（接线待 main.rs 仲裁） |
/// | `network.*` | AppState 构造期固化 ProcessManager network_env（模块子进程环境） |
///
/// 其余字段为运行期实时读取（执行闸门 / 下载闸门 / VRAM 预算 / EnvManager /
/// packs staging / active_models / language / theme 等），改动保存即生效。
fn restart_sensitive_changed(before: &AppConfig, after: &AppConfig) -> bool {
    before.server.host != after.server.host
        || before.server.port != after.server.port
        || before.ports.range_start != after.ports.range_start
        || before.ports.range_end != after.ports.range_end
        || before.pipeline.workspace_dir != after.pipeline.workspace_dir
        || before.compute.refresh_interval_secs != after.compute.refresh_interval_secs
        || before.compute.cuda_libs_dir != after.compute.cuda_libs_dir
        || before.general.log_level != after.general.log_level
        || before.network.http_proxy != after.network.http_proxy
        || before.network.https_proxy != after.network.https_proxy
        || before.network.no_proxy != after.network.no_proxy
}

/// PUT /api/config — 深度合并 JSON patch、持久化并更新内存共享配置（P1-9）。
///
/// 泛型请求体：路由注册为 `put_config::<Value>`（任意 patch 形状）；
/// 既有直调点可继续传 `Json<AppConfig>`（全量配置即"处处显式"的 patch，
/// 合并结果与旧的整替行为一致）。
///
/// 持有 config 写锁期间完成合并 + save()，避免并发读写不一致。
/// 落盘失败返回 500 + i18n 错误（apiCore.config.saveFailed，文案语言随
/// config.general.language；内存配置已合并，但持久化成功前重启会丢失）。
/// patch 非法返回 400 + i18n 错误（apiCore.config.invalidPatch），内存配置不变。
///
/// 注意：错误分支必须先释放写锁再调 [`err_response`] —— 其内部经
/// `state.lang()` 读 config，持写锁调用会在 tokio RwLock 上死锁。
pub async fn put_config<T>(
    State(state): State<Arc<AppState>>,
    Json(patch): Json<T>,
) -> Result<Json<PutConfigResponse>, (StatusCode, Json<Value>)>
where
    T: Serialize + serde::de::DeserializeOwned,
{
    let patch = match serde_json::to_value(&patch) {
        Ok(value) => value,
        Err(e) => {
            return Err(err_response(
                &state,
                StatusCode::BAD_REQUEST,
                "apiCore.config.invalidPatch",
                &[("detail", e.to_string())],
            )
            .await);
        }
    };

    let config_dir = state.root.join("config");

    enum Outcome {
        Merged {
            // Box 以平衡枚举变体尺寸（clippy::large_enum_variant）
            snapshot: Box<AppConfig>,
            requires_restart: bool,
        },
        InvalidPatch(anyhow::Error),
        SaveFailed(anyhow::Error),
    }

    let outcome = {
        let mut config = state.config.write().await;
        let before = config.clone();
        match config.merge_partial(&patch) {
            Ok(()) => {
                let requires_restart = restart_sensitive_changed(&before, &config);
                match config.save(&config_dir) {
                    Ok(()) => Outcome::Merged {
                        snapshot: Box::new(config.clone()),
                        requires_restart,
                    },
                    Err(e) => Outcome::SaveFailed(e),
                }
            }
            Err(e) => Outcome::InvalidPatch(e),
        }
    };

    match outcome {
        Outcome::Merged {
            snapshot,
            requires_restart,
        } => {
            tracing::debug!(requires_restart, "config patch merged and persisted");
            Ok(Json(PutConfigResponse {
                config: *snapshot,
                requires_restart,
            }))
        }
        Outcome::InvalidPatch(e) => {
            tracing::warn!(error = %e, "rejected invalid config patch");
            Err(err_response(
                &state,
                StatusCode::BAD_REQUEST,
                "apiCore.config.invalidPatch",
                &[("detail", e.to_string())],
            )
            .await)
        }
        Outcome::SaveFailed(e) => {
            tracing::error!(error = %e, "failed to persist config");
            Err(err_response(
                &state,
                StatusCode::INTERNAL_SERVER_ERROR,
                "apiCore.config.saveFailed",
                &[("detail", e.to_string())],
            )
            .await)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

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

    fn seq_state(language: &str) -> Arc<AppState> {
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "ep-api-config-{}-{seq}-{}",
            language,
            std::process::id()
        ));
        test_state(root, language)
    }

    /// 构造 PUT /config 请求（JSON patch 体）
    fn put_request(body: &str) -> Request<Body> {
        Request::builder()
            .method("PUT")
            .uri("/config")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    /// 走完整路由（含状态绑定）执行 PUT，返回状态码 + JSON 体
    async fn route_put(
        state: Arc<AppState>,
        body: &str,
    ) -> (StatusCode, serde_json::Value) {
        let app = router().with_state(state);
        let resp = app.oneshot(put_request(body)).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, json)
    }

    // ── 直调兼容（既有行为）────────────────────────────────────────────

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

        // put_config 会先将请求体合并进配置，错误语言取自合并后的配置，
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

    // 全量 AppConfig 直调（main.rs 既有调用形状）：合并 = 处处显式覆盖，行为同旧整替
    #[tokio::test]
    async fn put_config_full_body_replaces_like_before() {
        let state = seq_state("zh-CN");
        let mut new_config = AppConfig::default();
        new_config.general.language = "en-US".into();
        new_config.ports.range_start = 20000;

        let resp = put_config(State(state.clone()), Json(new_config))
            .await
            .expect("put_config should succeed");
        // Deref 到 AppConfig：旧式字段访问保持源码兼容
        assert_eq!(resp.0.general.language, "en-US");
        assert_eq!(resp.0.ports.range_start, 20000);
        // 未触及重启敏感项（20000 端口属 ports 段 → 敏感！此处 ports 确实变了）
        assert!(resp.0.requires_restart, "ports.range_start 变更应标记重启");

        let config = state.config.read().await;
        assert_eq!(config.general.language, "en-US");
    }

    // ── Router::oneshot：合并语义（P1-9）───────────────────────────────

    // 缺省保留：patch 未给出的字段一律不动（同表其他字段 + 其他段）
    #[tokio::test]
    async fn oneshot_put_missing_fields_preserved() {
        let state = seq_state("zh-CN");
        state.config.write().await.models.hf_endpoint = "https://mirror.example".into();

        let (status, body) =
            route_put(state.clone(), r#"{"general":{"language":"en"}}"#).await;
        assert_eq!(status, StatusCode::OK);

        // 显式字段覆盖
        assert_eq!(body["general"]["language"], "en");
        // 缺省字段保留原值
        assert_eq!(body["general"]["theme"], "dark");
        assert_eq!(body["models"]["hf_endpoint"], "https://mirror.example");
        assert_eq!(body["ports"]["range_start"], 18000);
        assert_eq!(body["requires_restart"], false);

        // 内存与磁盘一致
        assert_eq!(state.config.read().await.general.language, "en");
        let loaded = AppConfig::load(state.root.join("config").as_path()).expect("reload");
        assert_eq!(loaded.general.language, "en");
        assert_eq!(loaded.models.hf_endpoint, "https://mirror.example");
    }

    // 显式覆盖 + 嵌套合并：多段 patch 一次提交；未给出的子字段保留
    #[tokio::test]
    async fn oneshot_put_nested_merge_and_override() {
        let state = seq_state("zh-CN");

        let patch = r#"{
            "server": {"port": 9901},
            "compute": {"allow_overcommit": false, "refresh_interval_secs": 5},
            "python": {"constraints": ""}
        }"#;
        let (status, body) = route_put(state.clone(), patch).await;
        assert_eq!(status, StatusCode::OK);

        assert_eq!(body["server"]["port"], 9901);
        assert_eq!(body["server"]["host"], "0.0.0.0", "未显式给出的子字段保留");
        assert_eq!(body["compute"]["allow_overcommit"], false);
        assert_eq!(body["compute"]["refresh_interval_secs"], 5);
        assert_eq!(body["python"]["constraints"], "", "显式空串 = 停用，覆盖默认值");
        // server.port 属重启敏感项
        assert_eq!(body["requires_restart"], true);
    }

    // active_models 按键合并：已有键保留，显式键覆盖/新增
    #[tokio::test]
    async fn oneshot_put_active_models_per_key_merge() {
        let state = seq_state("zh-CN");
        state
            .config
            .write()
            .await
            .active_models
            .insert("mod-a".into(), "model-1".into());

        let (status, body) = route_put(
            state.clone(),
            r#"{"active_models":{"mod-b":"model-2"}}"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["active_models"]["mod-a"], "model-1", "已有键保留");
        assert_eq!(body["active_models"]["mod-b"], "model-2");
        assert_eq!(body["requires_restart"], false);

        // 第二次 patch 覆盖已有键
        let (status, body) = route_put(
            state.clone(),
            r#"{"active_models":{"mod-a":"model-9"}}"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["active_models"]["mod-a"], "model-9");
        assert_eq!(body["active_models"]["mod-b"], "model-2");
    }

    // 未知键忽略（仲裁 #7）：不报错，合法字段照常合并
    #[tokio::test]
    async fn oneshot_put_unknown_keys_ignored() {
        let state = seq_state("zh-CN");
        let (status, body) = route_put(
            state.clone(),
            r#"{"no_such_section":{"x":1},"general":{"no_such_field":42,"theme":"light"}}"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["general"]["theme"], "light");
    }

    // 重启敏感项判定：workspace 变更 → true；纯非敏感变更 → false
    #[tokio::test]
    async fn oneshot_put_requires_restart_flag() {
        let state = seq_state("zh-CN");

        let (status, body) =
            route_put(state.clone(), r#"{"models":{"hf_endpoint":"https://x"}}"#).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["requires_restart"], false);

        let (status, body) = route_put(
            state.clone(),
            r#"{"pipeline":{"workspace_dir":"workspace-2"}}"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["requires_restart"], true, "workspace_dir 为重启敏感项");

        let (status, body) = route_put(
            state.clone(),
            r#"{"network":{"http_proxy":"http://127.0.0.1:7890"}}"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body["requires_restart"], true,
            "network.* 为重启敏感项（ProcessManager 构造期固化）"
        );
    }

    // 非法 patch → 400，内存配置保持不变
    #[tokio::test]
    async fn oneshot_put_invalid_patch_400() {
        let state = seq_state("zh-CN");

        // a) JSON 语法错误（axum Json 提取器拒绝）
        let app = router().with_state(state.clone());
        let resp = app.oneshot(put_request("{not-json")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        // b) 字段类型不匹配 → merge_partial 拒绝，all-or-nothing
        let (status, body) = route_put(
            state.clone(),
            r#"{"ports":{"range_start":"not-a-number"},"general":{"theme":"light"}}"#,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body["error"].as_str().map(str::len).unwrap_or(0) > 0);
        let cfg = state.config.read().await;
        assert_eq!(cfg.ports.range_start, 18000, "非法 patch 不得部分写入");
        assert_eq!(cfg.general.theme, "dark");
        drop(cfg);

        // c) 非对象 patch（数组 / 标量）
        let (status, _) = route_put(state.clone(), "[1,2,3]").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = route_put(state.clone(), "42").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // d) 段形状错误（段应为表却给标量）
        let (status, _) = route_put(state.clone(), r#"{"general":"dark"}"#).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    // GET /config 不受 PUT 改造影响
    #[tokio::test]
    async fn oneshot_get_after_put() {
        let state = seq_state("zh-CN");
        let (_, body) = route_put(state.clone(), r#"{"general":{"language":"en"}}"#).await;
        assert_eq!(body["general"]["language"], "en");

        let app = router().with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/config")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["general"]["language"], "en");
        // GET 响应为纯 AppConfig，无 requires_restart 键
        assert!(json.get("requires_restart").is_none());
    }
}
