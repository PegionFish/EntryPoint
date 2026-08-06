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
    // 仅对声明 torch 依赖的模块检测 torch_cuda（任务 #10）：未声明 torch 的
    // 模块（如 faster-whisper/ctranslate2、rembg/onnxruntime）输出
    // "torch is not installed" 属误导。torch_cuda 字段恒在（可为空数组），
    // 响应 schema 保持向后兼容。
    let ids: Vec<&str> = modules
        .iter()
        .filter(|m| {
            let Some(mf) = m.manifest.as_ref() else {
                return false;
            };
            let req_rel = mf
                .runtime
                .requirements
                .as_deref()
                .unwrap_or("requirements.txt");
            let base = if m.path.is_absolute() {
                m.path.clone()
            } else {
                state.root.join(&m.path)
            };
            deps::requirements_declare_torch(&base.join(req_rel))
        })
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
        python_module_fixture_with_reqs(id, None)
    }

    /// 同 [`python_module_fixture`]，可额外写 requirements.txt（内容非空时落盘）
    fn python_module_fixture_with_reqs(
        id: &str,
        requirements: Option<&str>,
    ) -> DiscoveredModule {
        let req_line = if requirements.is_some() {
            "requirements = \"requirements.txt\"\n"
        } else {
            ""
        };
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
{req_line}
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

    // ── 任务 #10 回归：未声明 torch 的模块不输出 torch_cuda 项；
    //    声明 torch 的模块正常输出（字段恒在，schema 向后兼容）
    #[tokio::test]
    async fn deps_filters_modules_without_torch_declaration() {
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "ep-api-deps-torch-{}-{seq}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();

        // 两个模块都预置假 venv python（旧逻辑下两者都会产生 torch_cuda 项；
        // 假解释器使 check_torch_cuda 确定性失败 → 条目可产生）
        for id in ["torch-mod", "plain-mod"] {
            let py = if cfg!(target_os = "windows") {
                root.join(format!("runtime/venvs/{id}/Scripts/python.exe"))
            } else {
                root.join(format!("runtime/venvs/{id}/bin/python"))
            };
            std::fs::create_dir_all(py.parent().unwrap()).unwrap();
            std::fs::write(&py, b"fake").unwrap();
        }
        // requirements：torch-mod 声明 torch，plain-mod 仅 ctranslate2（不用 torch）
        std::fs::create_dir_all(root.join("modules/torch-mod")).unwrap();
        std::fs::write(
            root.join("modules/torch-mod/requirements.txt"),
            "torch==2.11.0\n",
        )
        .unwrap();
        std::fs::create_dir_all(root.join("modules/plain-mod")).unwrap();
        std::fs::write(
            root.join("modules/plain-mod/requirements.txt"),
            "ctranslate2>=4.0\n",
        )
        .unwrap();

        let state = Arc::new(AppState::new(
            root.clone(),
            AppConfig::default(),
            vec![],
            vec![
                python_module_fixture_with_reqs("torch-mod", Some("torch==2.11.0\n")),
                python_module_fixture_with_reqs("plain-mod", Some("ctranslate2>=4.0\n")),
            ],
            PortManager::new(18000, 19000),
        ));
        let (status, body) = get_deps(state).await;
        assert_eq!(status, axum::http::StatusCode::OK);
        assert!(body["torch_cuda"].is_array(), "torch_cuda 字段必须恒在");
        let arr = body["torch_cuda"].as_array().unwrap();
        assert_eq!(arr.len(), 1, "仅声明 torch 的模块应被检测");
        assert_eq!(arr[0]["module_id"], "torch-mod");
        assert!(arr[0]["cuda_available"].is_boolean());
        let _ = std::fs::remove_dir_all(&root);
    }
}
