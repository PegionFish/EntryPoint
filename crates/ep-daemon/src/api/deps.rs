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

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use serde_json::Value;
    use tower::ServiceExt;

    use ep_core::config::AppConfig;
    use ep_core::module::discovery::{DiscoveredModule, DiscoveryStatus};
    use ep_core::port::PortManager;

    use crate::state::AppState;

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    fn test_state(modules: Vec<DiscoveredModule>) -> Arc<AppState> {
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "ep-api-deps-test-{}-{seq}",
            std::process::id()
        ));
        Arc::new(AppState::new(
            root,
            AppConfig::default(),
            vec![],
            modules,
            PortManager::new(18000, 19000),
        ))
    }

    /// python 运行时模块 fixture（tempdir root 下无对应 venv）
    fn python_module_fixture(id: &str) -> DiscoveredModule {
        let manifest = toml::from_str(&format!(
            r#"
[module]
id = "{id}"
name = "Deps Fixture"
version = "0.1.0"
description = "deps test fixture"
category = "asr"
genre = "test"

[runtime]
type = "python"

[compute]
backends = ["cpu"]

[interface]
type = "http"
"#
        ))
        .unwrap();
        DiscoveredModule {
            path: std::path::PathBuf::from("modules").join(id),
            manifest: Some(manifest),
            status: DiscoveryStatus::Valid,
        }
    }

    async fn get_deps(state: Arc<AppState>) -> (axum::http::StatusCode, Value) {
        let app = super::router().with_state(state);
        let req = Request::builder().uri("/deps").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    // 无模块：报告形状稳定（ffmpeg 检测项恒在，torch_cuda 为空数组）。
    // ffmpeg 是否 available 依赖宿主机 PATH，不做断言（跨环境确定性）。
    #[tokio::test]
    async fn deps_report_shape_without_modules() {
        let (status, body) = get_deps(test_state(vec![])).await;
        assert_eq!(status, axum::http::StatusCode::OK);
        assert_eq!(body["ffmpeg"]["name"], "ffmpeg");
        assert!(body["ffmpeg"]["available"].is_boolean());
        assert_eq!(body["torch_cuda"], serde_json::json!([]));
    }

    // 模块无 venv python（tempdir 无 runtime/venvs/{id}）→ torch_cuda 仍为空；
    // manifest 无效的模块经 filter_map 跳过，不影响报告（不 panic、不产生条目）。
    #[tokio::test]
    async fn deps_skips_modules_without_venv_python() {
        let modules = vec![
            python_module_fixture("deps-mod-a"),
            python_module_fixture("deps-mod-b"),
            DiscoveredModule {
                path: std::path::PathBuf::from("modules/broken"),
                manifest: None,
                status: DiscoveryStatus::Invalid("missing module.toml".into()),
            },
        ];
        let (status, body) = get_deps(test_state(modules)).await;
        assert_eq!(status, axum::http::StatusCode::OK);
        assert_eq!(body["torch_cuda"], serde_json::json!([]));
    }
}
