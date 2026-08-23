//! Wave 4 **D1 E2E** — daemon 层端到端集成测试（Router::oneshot 全路由树）。
//!
//! # 覆盖矩阵（对应任务书 D1 条目）
//!
//! | 模块 | 覆盖 |
//! |---|---|
//! | `e2e_pack_chain` | 整合包全链：`ep_pack::import_pack` 导入（bundle+reference 混合）→ DELETE `/packs/{id}` 两分支（keep_models=false 清模型 / true 保留）+ 404 |
//! | `e2e_direct_exec` | 直跑链：`POST /upload/input` multipart → 返回路径 → `POST /execute/single`（校验全过 → 自动拉起失败 504）；`execution::submit_direct` 任务终态 + 错误路径 |
//! | `e2e_wait_callback` | `POST /pipelines/execute` `wait:true` → 200+status+artifacts（真实执行 file_input→file_output）；`callback_url` 本地 mock 端点捕获终态 POST；回调端点不可达不阻塞任务 |
//! | `e2e_vram_budget` | `POST /pipelines/vram-budget`：分层峰值 / over / unassigned / `allow_overcommit=false` 透传 |
//! | `e2e_gate_cancel` | `max_parallel=1` 两任务并发 → 一个 queued（队列位置经 `GET /pipelines/{id}/tasks` 可见）→ 首个完成后续运行；取消排队任务绝不执行 |
//! | `e2e_v1_inference` | v1 门面全链：`GET /v1/capabilities` 可查 → multipart 提交 202 → 轮询 `GET /v1/inference/result/{id}` 至终态 → 产物一律相对下载 URL（进程内 mock adapter 替代真实模块 HTTP） |
//! | `e2e_video_to_srt` | video-to-srt 条件回归（本机无 ffmpeg 无模块 venv → 打印原因跳过；测试体按 §15.1 Linux 已验证流程编写） |
//!
//! # harness 说明（ep-daemon 为纯 bin crate）
//!
//! ep-daemon 没有 lib 目标，且 `execution.rs` / `pipeline_bridge.rs` 文件头
//! 明确禁止在 main.rs 追加 `mod` 声明（同一文件双声明会分裂进程级静态注册表）。
//! 本集成测试沿用仓库既有的 `#[path]` 约定（见 `api/execute.rs`、
//! `api/pipelines.rs` 同款做法），在测试 crate 根重挂 `state` + `api`
//! 模块树，从而以 `api_router()` 全路由树做 Router::oneshot。
//!
//! **已知副作用**：被包含源文件内的 `#[cfg(test)]` 内联单测会随本集成测试
//! 二进制再跑一遍（它们自带 TEST_LOCK/tempdir 隔离，与本文件的测试互不干扰；
//! 全部触碰进程级静态的测试都先取 `execution::lock_for_tests()`）。
//!
//! # 环境受限声明
//!
//! 真实模块 HTTP 推理路径（module 节点 → `/predict/<capability>` 200 → 产物）
//! 依赖 Python venv + 真实模型，本机不可行——`e2e_direct_exec` 覆盖到
//! 提交/校验/自动拉起失败/任务终态错误路径；真实推理路径留待 Wave 5
//! 真机/真实环境复验（`e2e_video_to_srt` 为同一环境的条件回归）。

#[path = "../src/state.rs"]
mod state;
#[path = "../src/api/mod.rs"]
mod api;
// ws 模块是 state.log_tx/progress_tx/model_download_tx 的消费方之一，
// 纳入以保持与 main.rs 模块树对齐（缺之触发 dead_code 警告）
#[path = "../src/ws/mod.rs"]
mod ws;
// logging 模块被 api/config.rs 引用（PUT /api/config 的 log_level 动态
// reload 接线，P2-1），纳入以保持与 main.rs 模块树对齐
#[path = "../src/logging.rs"]
mod logging;
#[path = "../src/schedule.rs"]
mod schedule;

// ─── 公共 harness ────────────────────────────────────────────────────────────

mod common {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use axum::body::Body;
    use axum::http::{Method, Request, StatusCode};
    use axum::Router;
    use http_body_util::BodyExt;
    use serde_json::Value;
    use tower::ServiceExt;

    use ep_core::config::AppConfig;
    use ep_core::port::PortManager;
    use ep_core::types::{ComputeBackend, ComputeDevice, DeviceId};

    use crate::state::AppState;

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    /// 各测试独立 tempdir（双平台：一律 Path::join，不拼分隔符字面量）
    pub fn unique_root(tag: &str) -> PathBuf {
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "ep-e2e-{tag}-{}-{seq}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    pub fn test_state(root: PathBuf) -> std::sync::Arc<AppState> {
        std::sync::Arc::new(AppState::new(
            root,
            AppConfig::default(),
            vec![],
            vec![],
            PortManager::new(18000, 19000),
        ))
    }

    pub fn cuda_and_cpu_devices() -> Vec<ComputeDevice> {
        vec![
            ComputeDevice {
                id: DeviceId::Cuda(0),
                backend: ComputeBackend::Cuda,
                name: "E2E GPU".into(),
                total_memory_mb: Some(8192),
                used_memory_mb: None,
                utilization: None,
                temperature: None,
            },
            ComputeDevice {
                id: DeviceId::Cpu,
                backend: ComputeBackend::Cpu,
                name: "E2E CPU".into(),
                total_memory_mb: Some(16384),
                used_memory_mb: None,
                utilization: None,
                temperature: None,
            },
        ]
    }

    /// 挂载完整 /api 路由树（api_router 路由不带 /api 前缀，main.rs 才 nest）
    pub fn app(state: std::sync::Arc<AppState>) -> Router {
        crate::api::api_router(state.clone()).with_state(state)
    }

    pub fn json_request(method: Method, uri: &str, body: Value) -> Request<Body> {
        Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap()
    }

    pub fn delete_request(uri: &str) -> Request<Body> {
        Request::builder()
            .method(Method::DELETE)
            .uri(uri)
            .body(Body::empty())
            .unwrap()
    }

    pub fn get_request(uri: &str) -> Request<Body> {
        Request::builder().uri(uri).body(Body::empty()).unwrap()
    }

    pub async fn response_json(resp: axum::response::Response) -> (StatusCode, Value) {
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: Value = serde_json::from_slice(&bytes)
            .unwrap_or_else(|e| panic!("响应不是合法 JSON: {e}; body={bytes:?}"));
        (status, json)
    }

    /// Windows 路径 → TOML basic string 转义（反斜杠/引号）
    pub fn toml_path(p: &Path) -> String {
        p.display().to_string().replace('\\', "\\\\").replace('"', "\\\"")
    }

    pub fn write_file(path: &Path, content: &[u8]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, content).unwrap();
    }

    /// file_input → file_output 复制管线 TOML（真实可执行，路径内嵌）
    pub fn copy_pipeline_toml(id: &str, src: &Path, dest: &Path) -> String {
        format!(
            r#"
[pipeline]
id = "{id}"
name = "E2E 复制管线"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"
params = {{ path = "{}" }}

[[nodes]]
id = "output"
kind = "builtin"
builtin = "file_output"
params = {{ path = "{}" }}

[[edges]]
from = ["input", "output"]
to = ["output", "input"]
"#,
            toml_path(src),
            toml_path(dest),
        )
    }

    /// 轮询等待任务终态（60s 预算）
    pub async fn wait_terminal(task_id: &str) -> Option<ep_core::task_registry::TaskRecord> {
        for _ in 0..1200 {
            if let Some(record) = crate::api::execute::execution::snapshot(task_id) {
                if record.status.is_terminal() {
                    return Some(record);
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        None
    }

    /// 轮询等待任务进入指定状态（10s 预算）
    pub async fn wait_status(
        task_id: &str,
        want: ep_core::task_registry::TaskState,
    ) -> Option<ep_core::task_registry::TaskRecord> {
        for _ in 0..200 {
            if let Some(record) = crate::api::execute::execution::snapshot(task_id) {
                if record.status == want {
                    return Some(record);
                }
                if record.status.is_terminal() && want != record.status {
                    return None;
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        None
    }

    /// 跨平台保活命令（fake 模块 start_command：自动拉起路径需要存活子进程，
    /// 但端口上无服务监听 → 健康探测必然失败）
    pub fn keepalive_command() -> String {
        if cfg!(target_os = "windows") {
            "ping -n 30 127.0.0.1 > NUL".to_string()
        } else {
            "sleep 30".to_string()
        }
    }

    /// harness teardown：停止 state 内所有运行中/启动中的模块子进程并释放
    /// 其端口。
    ///
    /// E2E 以进程内 `Router::oneshot` 驱动，**不经过** main.rs `run_server`
    /// 的优雅退出路径（Ctrl+C → stop_all_modules）；若不显式回收，自动拉起
    /// 的模块进程在测试结束后成为孤儿并占用端口（实测：faster-whisper
    /// adapter 残留监听 0.0.0.0:18000）。语义对齐 main.rs stop_all_modules
    /// 与停止端点的端口释放：逐个停止（stop_module 内部含进程树回收），
    /// 单个失败仅告警不阻断。
    pub async fn stop_all_running_modules(state: &std::sync::Arc<AppState>) {
        let running: Vec<String> = {
            let pm = state.process_manager.read().await;
            pm.list_running()
                .iter()
                .map(|inst| inst.module_id.clone())
                .collect()
        };
        for module_id in running {
            let stop_result = {
                let mut pm = state.process_manager.write().await;
                pm.stop_module(&module_id).await
            };
            match stop_result {
                Ok(()) => {
                    state.port_manager.write().await.release(&module_id);
                }
                Err(e) => {
                    eprintln!("teardown: 停止模块 {module_id} 失败: {e}");
                }
            }
        }
    }

    // ── multipart 构造（/upload/input 字段名 file，仲裁 #3） ──────────────

    pub const BOUNDARY: &str = "----ep-e2e-boundary";

    pub fn multipart_body_with_file(file_name: &str, data: &[u8]) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
        buf.extend_from_slice(
            format!(
                "Content-Disposition: form-data; name=\"file\"; filename=\"{file_name}\"\r\n\
                 Content-Type: application/octet-stream\r\n\r\n"
            )
            .as_bytes(),
        );
        buf.extend_from_slice(data);
        buf.extend_from_slice(b"\r\n");
        buf.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
        buf
    }

    pub fn multipart_request(uri: &str, body: Vec<u8>) -> Request<Body> {
        Request::builder()
            .method(Method::POST)
            .uri(uri)
            .header(
                "content-type",
                format!("multipart/form-data; boundary={BOUNDARY}"),
            )
            .header("content-length", body.len().to_string())
            .body(Body::from(body))
            .unwrap()
    }

    /// ServiceExt::oneshot 便捷包装
    pub async fn oneshot(
        router: Router,
        req: Request<Body>,
    ) -> axum::response::Response {
        router.oneshot(req).await.unwrap()
    }
}

// ─── 0. 模块树装配冒烟（ws_router 消费 state 三通道，防 harness 装配漂移） ──

#[tokio::test]
async fn ws_router_wires_state_channels() {
    use tower::ServiceExt;

    let root = common::unique_root("ws-smoke");
    let state = common::test_state(root);
    let router = crate::ws::ws_router().with_state(state.clone());
    // GET /ws 升级为 WebSocket 握手；oneshot 无升级头 → 非 200 亦非 panic，
    // 证明路由 + 状态装配可用（真实 WS 会话不在 E2E 范围）
    let resp = router.oneshot(common::get_request("/ws")).await.unwrap();
    assert_ne!(resp.status(), axum::http::StatusCode::NOT_FOUND);
}

// ─── 1. 整合包全链 E2E：import_pack → DELETE 两分支 ─────────────────────────

mod e2e_pack_chain {
    //! 任务书条目 1：构建测试包（bundle + reference 混合 + 管线）→
    //! `ep_pack::import::import_pack` 导入 → 断言 meta/注册表/管线落位 →
    //! 二次导入拒绝 → DELETE 语义（keep_models 两分支，Router::oneshot）。
    //!
    //! reference 下载完成后的 meta 补丁（仲裁 #21）由 `packs.rs` 私有函数
    //! `patch_reference_meta` 执行，其单元覆盖在 packs.rs 内联测试
    //! （reference_meta_patch_sets_pack_id / _missing_meta_is_noop，随本
    //! 二进制重跑）；真实下载链需 Python venv，本机不可行（Wave 5 复验）。
    //! 本模块以「补丁后的 meta 形状」预置 reference 模型目录，端到端验证
    //! DELETE 按 `meta.pack_id` 的扫描能覆盖 bundle + 后下载 reference 两者。

    use std::path::Path;

    use serde_json::json;
    use tower::ServiceExt;

    use ep_core::model::{ModelManager, ModelMeta};
    use ep_pack::build::{build_pack, BuildPlan};
    use ep_pack::import::{
        import_pack, ImportOptions, ImportTargets, PendingDownload, ResolvedModel,
    };
    use ep_pack::manifest::PackModelEntry;

    use crate::common::*;

    const PACK_MANIFEST: &str = r#"
[pack]
id = "tester.e2e-pack"
version = "1.0.0"
name = "E2E Pack"
description = "wave-4 d1 e2e fixture"
authors = ["d1"]
min_ep_version = "0.1.0"
tags = ["e2e"]

[compute]
backends = ["cuda", "cpu"]

[[models]]
qualified_id = "ep.acme.asr"
variant = "v1"
mode = "bundle"
tags = ["asr"]

[[models]]
qualified_id = "ep.acme.tts"
variant = "v2"
mode = "reference"

[[pipelines]]
file = "pipelines/main.toml"
"#;

    const PIPELINE_MAIN: &str = r#"
[pipeline]
id = "e2e-main"
name = "E2E Main"

[[nodes]]
id = "asr"
kind = "module"
module_id = "asr"
capability = "transcribe"
model = "ep.acme.asr@v1"
"#;

    fn resolver() -> impl Fn(&PackModelEntry) -> Result<ResolvedModel, String> {
        |entry: &PackModelEntry| match (entry.qualified_id.as_str(), entry.variant.as_str()) {
            ("ep.acme.asr", "v1") => Ok(ResolvedModel {
                module_id: "asr".into(),
                model_id: "v1".into(),
                target_dir: "asr-v1".into(),
                backends: vec![
                    ep_core::types::ComputeBackend::Cuda,
                    ep_core::types::ComputeBackend::Cpu,
                ],
                download: None,
            }),
            ("ep.acme.tts", "v2") => Ok(ResolvedModel {
                module_id: "tts".into(),
                model_id: "v2".into(),
                target_dir: "tts-v2".into(),
                backends: vec![ep_core::types::ComputeBackend::Cpu],
                download: Some(PendingDownload {
                    source: "huggingface".into(),
                    location: "acme/tts-v2".into(),
                    revision: Some("main".into()),
                }),
            }),
            (qid, variant) => Err(format!("module for {qid}@{variant} is not installed")),
        }
    }

    /// build → import 全链；返回 (root, targets)
    fn import_fixture(root: &Path) -> ImportTargets {
        let src = root.join("pack-src");
        write_file(&src.join("ep-pack.toml"), PACK_MANIFEST.as_bytes());
        write_file(
            &src.join("models").join("asr-v1").join("weights.bin"),
            b"pseudo-weights-bytes",
        );
        write_file(
            &src.join("pipelines").join("main.toml"),
            PIPELINE_MAIN.as_bytes(),
        );
        let archive = root.join("e2e-pack.epzip");
        build_pack(&BuildPlan::new(&src, &archive)).unwrap();

        let targets = ImportTargets::from_root(root);
        let report = import_pack(
            &archive,
            &root.join(".pack-staging"),
            &targets,
            &ImportOptions::default(),
            &cuda_and_cpu_devices(),
            resolver(),
            |p| {
                // 进度阶段有序性在 ep-pack tests/import_flow.rs 全量断言；
                // 此处仅消费（E2E 链路经过进度回调不 panic）
                assert!(p.percent <= 100);
            },
        )
        .expect("测试包导入应成功");

        // 导入侧关键断言：meta / 注册表 / 管线落位
        assert_eq!(report.pack_id, "tester.e2e-pack");
        let bundle_dir = targets.models_dir.join("asr-v1");
        assert!(bundle_dir.join("weights.bin").is_file());
        let meta: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(bundle_dir.join(".ep_meta.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(meta["source"], "pack");
        assert_eq!(meta["pack_id"], "tester.e2e-pack");
        assert_eq!(meta["qualified_id"], "ep.acme.asr");
        assert!(targets.pipelines_dir.join("main.toml").is_file());
        assert!(report.registry_path.is_file());
        assert_eq!(report.pending_downloads.len(), 1, "reference → 待下载描述符");
        targets
    }

    /// 预置「reference 下载完成 + meta 补丁后」的模型目录。
    ///
    /// 补丁语义（pack_id/qualified_id/tags 回填）由 daemon 私有函数执行
    /// （packs.rs 内联测试覆盖）；此处直接产出补丁后的终态 meta，
    /// 供 DELETE 的 `meta.pack_id` 扫描端到端消费。
    fn seed_patched_reference_model(root: &Path) {
        let dir = root.join("models").join("tts-v2");
        write_file(&dir.join("weights.bin"), b"downloaded-weights");
        let meta = ModelMeta {
            module_id: "tts".into(),
            model_id: "v2".into(),
            source: "huggingface".into(),
            repo_id: "acme/tts-v2".into(),
            revision: "main".into(),
            downloaded_at: chrono::Utc::now().to_rfc3339(),
            total_size_bytes: 18,
            qualified_id: Some("ep.acme.tts".into()),
            tags: vec!["e2e".into()],
            pack_id: Some("tester.e2e-pack".into()),
        };
        let mgr = ModelManager::new(&ep_core::config::ModelsConfig::default(), root);
        mgr.write_meta("tts-v2", &meta).unwrap();
        assert!(mgr.is_model_present("tts-v2"));
    }

    #[tokio::test]
    async fn delete_pack_keep_models_false_removes_models_pipelines_registry() {
        let root = unique_root("pack-del");
        let targets = import_fixture(&root);
        seed_patched_reference_model(&root);

        let state = test_state(root.clone());
        let resp = app(state.clone())
            .oneshot(delete_request("/packs/tester.e2e-pack"))
            .await
            .unwrap();
        let (status, body) = response_json(resp).await;
        assert_eq!(status, axum::http::StatusCode::OK, "{body}");
        assert_eq!(body["ok"], true);

        // bundle 模型目录 + 补丁后的 reference 目录均被删除
        assert!(!targets.models_dir.join("asr-v1").exists());
        assert!(!targets.models_dir.join("tts-v2").exists());
        // 管线落位文件被删除
        assert!(!targets.pipelines_dir.join("main.toml").exists());
        // 注册表条目被删除 → GET /packs 空列表
        assert!(!targets.registry_dir.join("tester.e2e-pack.json").exists());
        let resp = oneshot(app(state), get_request("/packs")).await;
        let (status, body) = response_json(resp).await;
        assert_eq!(status, axum::http::StatusCode::OK);
        assert_eq!(body, json!([]));
    }

    #[tokio::test]
    async fn delete_pack_keep_models_true_keeps_models() {
        let root = unique_root("pack-keep");
        let targets = import_fixture(&root);
        seed_patched_reference_model(&root);

        let state = test_state(root.clone());
        let resp = app(state.clone())
            .oneshot(delete_request("/packs/tester.e2e-pack?keep_models=true"))
            .await
            .unwrap();
        let (status, body) = response_json(resp).await;
        assert_eq!(status, axum::http::StatusCode::OK, "{body}");

        // 模型保留（bundle + reference 均在）
        assert!(targets.models_dir.join("asr-v1").join("weights.bin").is_file());
        assert!(targets.models_dir.join("tts-v2").join("weights.bin").is_file());
        // 注册表与管线仍被删除（keep_models 只影响模型）
        assert!(!targets.registry_dir.join("tester.e2e-pack.json").exists());
        assert!(!targets.pipelines_dir.join("main.toml").exists());
    }

    #[tokio::test]
    async fn delete_unknown_pack_404() {
        let root = unique_root("pack-404");
        let state = test_state(root);
        let resp = oneshot(app(state), delete_request("/packs/tester.ghost")).await;
        let (status, body) = response_json(resp).await;
        assert_eq!(status, axum::http::StatusCode::NOT_FOUND);
        assert!(body["error"].is_string());
    }

    #[tokio::test]
    async fn import_twice_rejected_then_delete_allows_reimport() {
        // 整合包生命周期 E2E：导入 → 二次导入拒绝（PackAlreadyInstalled 在
        // ep-pack tests 断言）→ DELETE → 注册表清空（此后重新导入可行，
        // 与仲裁 #17「先卸载再导入」语义自洽）
        let root = unique_root("pack-life");
        let targets = import_fixture(&root);

        let state = test_state(root.clone());
        let app = app(state.clone());
        let resp = app
            .oneshot(delete_request("/packs/tester.e2e-pack"))
            .await
            .unwrap();
        let (status, _) = response_json(resp).await;
        assert_eq!(status, axum::http::StatusCode::OK);

        // 卸载后注册表为空 → 再导入成功（全链可重复）
        assert!(!targets.registry_dir.join("tester.e2e-pack.json").exists());
        std::fs::remove_dir_all(root.join("models")).ok();
        let _ = import_fixture(&root);
        assert!(targets.registry_dir.join("tester.e2e-pack.json").is_file());
    }
}

// ─── 2. 直跑 E2E：upload/input → execute/single + submit_direct 终态 ────────

mod e2e_direct_exec {
    //! 任务书条目 2：真实模块 HTTP 拉起在本机无 Python venv 环境不可行——
    //! 用 fake 进程能力（keepalive start_command + 1s 健康超时）覆盖
    //! 上传 → 提交 → 校验 → 自动拉起失败路径；任务终态 + 错误经
    //! `execution::submit_direct` 全链验证。真实推理路径（模块 200 →
    //! 产物）需真实环境，见文件头环境受限声明。
    // 测试锁（execution::TEST_LOCK）跨 await 串行化共享静态注册表测试，
    // 锁内临界区全部是极短同步操作，不存在持锁阻塞运行时的风险
    //（与 execution/execute/pipelines 内联测试模块同款豁免）。
    #![allow(clippy::await_holding_lock)]

    use std::path::PathBuf;
    use std::sync::Arc;

    use serde_json::json;

    use ep_core::config::AppConfig;
    use ep_core::module::discovery::{DiscoveredModule, DiscoveryStatus};
    use ep_core::module::manifest::ModuleManifest;
    use ep_core::port::PortManager;

    use crate::api::execute::execution;
    use crate::common::*;
    use crate::state::AppState;

    /// fake 直跑模块：native 运行时 + 跨平台保活命令（进程存活但端口上
    /// 无服务 → 健康探测必然失败）；`ready_timeout_secs = 1` 加速失败路径。
    fn fake_module_manifest() -> ModuleManifest {
        toml::from_str(&format!(
            r#"
[module]
id = "e2e-mod"
name = "E2E 直跑模块"
version = "0.1.0"
description = "wave-4 d1 fixture"
category = "asr"
genre = "test"

[runtime]
type = "native"
binaries = {{ "test" = "test" }}
start_command = "{}"

[compute]
backends = ["cpu"]

[interface]
type = "http"
health_endpoint = "/health"
ready_timeout_secs = 1

[[interface.capabilities]]
name = "run"
description = "run it"
input_type = "file"
output_type = "file"

[interface.capabilities.params]
beam_size = {{ type = "integer", min = 1, max = 20 }}
"#,
            keepalive_command()
        ))
        .unwrap()
    }

    fn state_with_fake_module(root: PathBuf) -> Arc<AppState> {
        let module = DiscoveredModule {
            path: root.join("modules").join("e2e-mod"),
            manifest: Some(fake_module_manifest()),
            status: DiscoveryStatus::Valid,
        };
        Arc::new(AppState::new(
            root,
            AppConfig::default(),
            vec![],
            vec![module],
            // 独立区间（避开生产默认 18000-19000）：本测试断言"无服务响应
            // /health → 504"，若与并发真实 daemon 的 adapter 端口区间重叠，
            // 探测可能命中真实 /health 返回 200，误判模块就绪（环境性 flake）。
            PortManager::new(48300, 48320),
        ))
    }

    /// /api/upload/input 上传 → 返回路径 → /api/execute/single 用该路径提交：
    /// 校验全过 → 模块自动拉起 → 无 venv/无服务 → 504 健康超时（失败清理完成）
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn upload_input_then_execute_single_full_chain() {
        let root = unique_root("upload-single");
        let state = state_with_fake_module(root.clone());
        let app = app(state.clone());

        // 1) multipart 上传直跑输入
        let payload = b"e2e direct exec input payload";
        let resp = oneshot(
            app.clone(),
            multipart_request(
                "/upload/input",
                multipart_body_with_file("e2e-input.txt", payload),
            ),
        )
        .await;
        let (status, body) = response_json(resp).await;
        assert_eq!(status, axum::http::StatusCode::OK, "{body}");
        let uploaded = PathBuf::from(body["path"].as_str().expect("path 字段"));
        assert!(uploaded.is_file(), "上传文件应落盘: {}", uploaded.display());
        assert_eq!(std::fs::read(&uploaded).unwrap(), payload);
        // 落位 workspace/uploads（§8.1 暂存约定）
        let uploads_dir = AppConfig::default().resolve_workspace_dir(&root).join("uploads");
        assert!(
            uploaded.starts_with(&uploads_dir),
            "uploaded {} should be under {}",
            uploaded.display(),
            uploads_dir.display()
        );

        // 2) 直跑提交：校验全过（模块/capability/参数/输入文件）→ 自动拉起
        //    fake 进程存活但无健康端点 → ready_timeout_secs=1 → 504
        let resp = oneshot(
            app,
            json_request(
                axum::http::Method::POST,
                "/execute/single",
                json!({
                    "module_id": "e2e-mod",
                    "capability": "run",
                    "params": { "beam_size": 5 },
                    "input_path": uploaded.display().to_string()
                }),
            ),
        )
        .await;
        let (status, body) = response_json(resp).await;
        assert_eq!(
            status,
            axum::http::StatusCode::GATEWAY_TIMEOUT,
            "真实模块拉起不可行（无 venv）→ 等健康超时 504: {body}"
        );
        assert!(
            body["error"].as_str().unwrap().contains("e2e-mod"),
            "{}",
            body
        );

        // 3) 失败清理：模块停止 + 端口释放
        {
            let pm = state.process_manager.read().await;
            assert_eq!(
                pm.get_status("e2e-mod"),
                Some(&ep_core::types::ServiceStatus::Stopped)
            );
        }
        assert!(state.port_manager.read().await.get_port("e2e-mod").is_none());
    }

    /// execution::submit_direct 全链：校验通过 → 入闸 → start_task 自动拉起
    /// 失败 → 任务终态 Failed（错误携带原因，产物为空）
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn submit_direct_task_reaches_terminal_failed_without_module_env() {
        let _guard = execution::lock_for_tests();
        execution::clear_registry_for_tests();

        let root = unique_root("direct-term");
        let state = state_with_fake_module(root.clone());
        let input = root.join("direct-in.txt");
        write_file(&input, b"direct e2e");

        let task_id = execution::submit_direct(
            &state,
            "e2e-mod",
            "run",
            json!({ "beam_size": 3 }),
            input,
        )
        .await
        .expect("提交期校验应通过");
        assert!(task_id.starts_with("task-"));

        let record = wait_terminal(&task_id).await.expect("任务应终结");
        assert_eq!(
            record.status,
            execution::TaskState::Failed,
            "无模块运行环境 → 自动拉起失败计入任务错误"
        );
        let error = record.error.expect("失败任务携带错误");
        assert!(error.contains("e2e-mod"), "{error}");
        assert!(record.artifacts.is_empty(), "失败任务无产物");
        assert_eq!(record.pipeline_id, "direct/e2e-mod", "直跑 pipeline_id 形状");
    }
}

// ─── 2b. v1 统一推理门面全链 E2E：capabilities → 提交 → 轮询 → 相对产物 URL ──

mod e2e_v1_inference {
    //! v1 门面全链用例（任务书 §5 补充条目）：本机无 Python venv/真实模型，
    //! 用进程内 mock adapter（在 PortManager 分配端口上提供 `/health` +
    //! `/predict/run`）替代真实模块 HTTP 服务，keepalive 子进程保活
    //! ProcessManager 实例（与 autostart.rs 内联测试同款模式，序列约束：
    //! 先 allocate 再起 mock）；其余链路（上传落盘 → 校验序列 → 自动拉起 →
    //! submit_direct_full → 引擎真实执行 → 产物归集）全部走生产代码。
    // 测试锁跨 await 串行化（同 e2e_direct_exec 注释）
    #![allow(clippy::await_holding_lock)]

    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;

    use axum::extract::Multipart;
    use axum::response::IntoResponse;
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use serde_json::Value;

    use ep_core::config::AppConfig;
    use ep_core::module::discovery::{DiscoveredModule, DiscoveryStatus};
    use ep_core::module::manifest::ModuleManifest;
    use ep_core::port::PortManager;

    use crate::api::execute::execution;
    use crate::common::*;
    use crate::state::AppState;

    /// fake v1 模块：native 运行时 + keepalive 命令（进程存活，端口 HTTP
    /// 服务由进程内 mock adapter 承担）；capability `run`（file → file）
    fn v1_module_manifest() -> ModuleManifest {
        toml::from_str(&format!(
            r#"
[module]
id = "e2e-v1-mod"
name = "E2E v1 门面模块"
version = "0.1.0"
description = "v1 e2e fixture"
category = "asr"
genre = "test"

[runtime]
type = "native"
binaries = {{ "test" = "test" }}
start_command = "{}"

[compute]
backends = ["cpu"]

[interface]
type = "http"
health_endpoint = "/health"
ready_timeout_secs = 15

[[interface.capabilities]]
name = "run"
description = "run it"
input_type = "file"
output_type = "file"
"#,
            keepalive_command()
        ))
        .unwrap()
    }

    fn v1_state(root: PathBuf) -> Arc<AppState> {
        let module = DiscoveredModule {
            path: root.join("modules").join("e2e-v1-mod"),
            manifest: Some(v1_module_manifest()),
            status: DiscoveryStatus::Valid,
        };
        Arc::new(AppState::new(
            root,
            AppConfig::default(),
            vec![],
            vec![module],
            // 独立区间：避开 e2e_direct_exec 的 48300-48320 与生产默认段
            PortManager::new(48400, 48420),
        ))
    }

    /// 进程内 mock adapter：`GET /health` → 200；`POST /predict/run` →
    /// 解析 multipart（file + params）→ 写转换产物 → 返回 ModuleResponse
    /// 形状（status/output_type/result）。产物路径优先用执行器注入的
    /// `output_path`（仅当节点参数含 output_format 时注入），缺省时
    /// 自派生临时路径——executor 只信任 result 返回的路径字符串。
    /// 与 ep-core executor 模块调用契约（`/predict/{capability}` multipart
    /// 投递）逐点对齐。
    async fn spawn_mock_adapter(port: u16) -> tokio::task::JoinHandle<()> {
        async fn predict(mut multipart: Multipart) -> impl IntoResponse {
            let mut output_path: Option<String> = None;
            let mut file_bytes: Vec<u8> = Vec::new();
            while let Ok(Some(field)) = multipart.next_field().await {
                match field.name().unwrap_or("") {
                    "params" => {
                        let text = field.text().await.unwrap_or_default();
                        if let Ok(v) = serde_json::from_str::<Value>(&text) {
                            output_path = v
                                .get("output_path")
                                .and_then(|p| p.as_str())
                                .map(String::from);
                        }
                    }
                    "file" => {
                        file_bytes = field.bytes().await.unwrap_or_default().to_vec();
                    }
                    _ => {}
                }
            }
            // 无 output_path 注入时自派生（进程独占 tempdir，不与其他测试冲突）
            let out = output_path.unwrap_or_else(|| {
                std::env::temp_dir()
                    .join(format!("ep-v1-mock-{}", std::process::id()))
                    .join(format!(
                        "mock-out-{}.txt",
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_nanos())
                            .unwrap_or(0)
                    ))
                    .display()
                    .to_string()
            });
            if let Some(parent) = std::path::Path::new(&out).parent() {
                std::fs::create_dir_all(parent).expect("mock 产物目录");
            }
            let content = format!("v1-e2e-out({})", String::from_utf8_lossy(&file_bytes));
            std::fs::write(&out, content).expect("mock 产物写盘");
            Json(serde_json::json!({
                "status": "completed",
                "output_type": "file",
                "result": out,
            }))
        }

        let app = Router::new()
            .route("/health", get(|| async { "OK" }))
            .route("/predict/run", post(predict));
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
            .await
            .unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        })
    }

    /// v1 全链：capabilities 可查 → multipart 提交 202 → 轮询 result 至
    /// completed → 产物 URL 一律相对路径（`/api/tasks/{id}/artifacts/{node}`）
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn v1_full_chain_capabilities_submit_poll_relative_urls() {
        let _guard = execution::lock_for_tests();
        execution::clear_registry_for_tests();

        let root = unique_root("v1-chain");
        let state = v1_state(root.clone());

        // 先 allocate（OS 占用探测在 mock bind 前）→ 再在同端口起 mock
        let port = state
            .port_manager
            .write()
            .await
            .allocate("e2e-v1-mod")
            .expect("预分配端口");
        let adapter = spawn_mock_adapter(port).await;

        // 1) capabilities 可查：形状 + 字段值
        let resp = oneshot(app(state.clone()), get_request("/v1/capabilities")).await;
        let (status, body) = response_json(resp).await;
        assert_eq!(status, axum::http::StatusCode::OK, "{body}");
        let caps = body["capabilities"].as_array().expect("capabilities 列表");
        assert_eq!(caps.len(), 1, "{body}");
        assert_eq!(caps[0]["module_id"], "e2e-v1-mod");
        assert_eq!(caps[0]["capability"], "run");
        assert_eq!(caps[0]["input_type"], "file");
        assert_eq!(caps[0]["output_type"], "file");

        // 2) multipart 提交（file 字段，wait 缺省 false）→ 202 {task_id}
        let resp = oneshot(
            app(state.clone()),
            multipart_request(
                "/v1/inference/e2e-v1-mod/run",
                multipart_body_with_file("v1-in.txt", b"v1 e2e payload"),
            ),
        )
        .await;
        let (status, body) = response_json(resp).await;
        assert_eq!(status, axum::http::StatusCode::ACCEPTED, "{body}");
        let task_id = body["task_id"].as_str().expect("task_id").to_string();
        assert!(task_id.starts_with("task-"));

        // 3) 轮询 result 至终态（20s 预算）
        let mut last = Value::Null;
        let mut completed = false;
        for _ in 0..400 {
            let resp = oneshot(
                app(state.clone()),
                get_request(&format!("/v1/inference/result/{task_id}")),
            )
            .await;
            let (status, body) = response_json(resp).await;
            assert_eq!(status, axum::http::StatusCode::OK, "{body}");
            last = body.clone();
            match body["status"].as_str().unwrap_or("") {
                "completed" => {
                    completed = true;
                    break;
                }
                "failed" | "cancelled" => break,
                _ => tokio::time::sleep(Duration::from_millis(50)).await,
            }
        }
        assert!(completed, "v1 推理应真实执行完成: {last}");

        // 4) 产物一律相对下载 URL（绝不回传服务器绝对路径），output 节点在列
        let outputs = last["outputs"].as_array().expect("outputs 列表");
        assert!(!outputs.is_empty(), "{last}");
        for o in outputs {
            let url = o["url"].as_str().expect("url 字段");
            assert!(
                url.starts_with(&format!("/api/tasks/{task_id}/artifacts/")),
                "产物必须为相对下载 URL: {url}"
            );
        }
        assert!(
            outputs.iter().any(|o| o["node_id"] == "output"),
            "output 节点产物应在列: {last}"
        );

        // teardown：停止 keepalive 子进程 + abort mock adapter
        stop_all_running_modules(&state).await;
        adapter.abort();
    }

    /// multipart body（file + wait 两字段）：m1 wait=true HTTP 层用例专用
    fn multipart_body_with_file_and_wait(file_name: &str, data: &[u8]) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
        buf.extend_from_slice(
            format!(
                "Content-Disposition: form-data; name=\"file\"; filename=\"{file_name}\"\r\n\
                 Content-Type: application/octet-stream\r\n\r\n"
            )
            .as_bytes(),
        );
        buf.extend_from_slice(data);
        buf.extend_from_slice(b"\r\n");
        buf.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
        buf.extend_from_slice(b"Content-Disposition: form-data; name=\"wait\"\r\n\r\ntrue");
        buf.extend_from_slice(b"\r\n");
        buf.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
        buf
    }

    /// m1：wait=true multipart → 200 + status=completed + output_url 相对路径前缀
    ///（HTTP 层同步路径覆盖，与上方异步轮询链互补）
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn v1_wait_true_multipart_returns_completed_with_relative_output_url() {
        let _guard = execution::lock_for_tests();
        execution::clear_registry_for_tests();

        let root = unique_root("v1-wait");
        let state = v1_state(root.clone());

        // 先 allocate（OS 占用探测在 mock bind 前）→ 再在同端口起 mock
        let port = state
            .port_manager
            .write()
            .await
            .allocate("e2e-v1-mod")
            .expect("预分配端口");
        let adapter = spawn_mock_adapter(port).await;

        let resp = oneshot(
            app(state.clone()),
            multipart_request(
                "/v1/inference/e2e-v1-mod/run",
                multipart_body_with_file_and_wait("v1-wait.txt", b"v1 wait payload"),
            ),
        )
        .await;
        let (status, body) = response_json(resp).await;
        assert_eq!(status, axum::http::StatusCode::OK, "wait=true → 200: {body}");
        assert!(
            body["task_id"].as_str().unwrap_or("").starts_with("task-"),
            "{body}"
        );
        assert_eq!(body["status"], "completed", "{body}");
        let url = body["output_url"].as_str().expect("output_url 应携带");
        assert!(
            url.starts_with("/api/tasks/"),
            "output_url 必须为相对下载 URL 前缀: {url}"
        );

        // teardown：停止 keepalive 子进程 + abort mock adapter
        stop_all_running_modules(&state).await;
        adapter.abort();
    }
}

// ─── 3. wait/callback E2E（§6.5）─────────────────────────────────────────────

mod e2e_wait_callback {
    //! 任务书条目 3：POST /api/pipelines/execute wait:true（file_input→
    //! file_output 纯 builtin 管线，真实执行）→ 200+status+artifacts；
    //! callback_url 用本地 mock axum 端点捕获终态 POST。
    // 测试锁跨 await 串行化（同 e2e_direct_exec 注释）
    #![allow(clippy::await_holding_lock)]

    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use serde_json::{json, Value};

    use crate::api::execute::execution;
    use crate::common::*;

    /// 写一条 file_input→file_output 管线到 config/pipelines（path 经 inputs 注入）
    fn write_pipe_file(root: &std::path::Path, id: &str) {
        let toml = format!(
            r#"
[pipeline]
id = "{id}"
name = "wait/callback E2E"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"

[[nodes]]
id = "output"
kind = "builtin"
builtin = "file_output"

[[edges]]
from = ["input", "output"]
to = ["output", "input"]
"#
        );
        write_file(&root.join("config").join("pipelines").join(format!("{id}.toml")), toml.as_bytes());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn wait_true_returns_terminal_status_and_artifacts() {
        let _guard = execution::lock_for_tests();
        execution::clear_registry_for_tests();

        let root = unique_root("wait-http");
        write_pipe_file(&root, "wait-pipe");
        let src = root.join("w-src.txt");
        let dest = root.join("w-out.txt");
        write_file(&src, b"wait e2e payload");

        let state = test_state(root.clone());
        let resp = oneshot(
            app(state),
            json_request(
                axum::http::Method::POST,
                "/pipelines/execute",
                json!({
                    "pipeline_id": "wait-pipe",
                    "wait": true,
                    "inputs": {
                        "input": { "path": src.display().to_string() },
                        "output": { "path": dest.display().to_string() }
                    }
                }),
            ),
        )
        .await;
        let (status, body) = response_json(resp).await;
        assert_eq!(status, axum::http::StatusCode::OK, "wait 模式 → 200: {body}");
        assert!(body["task_id"].as_str().unwrap().starts_with("task-"));
        assert_eq!(body["status"], "completed", "{body}");
        let artifacts = body["artifacts"].as_array().expect("artifacts 清单");
        assert_eq!(artifacts.len(), 2, "{body}");
        for a in artifacts {
            assert!(a["node_id"].is_string());
            assert!(
                std::path::Path::new(a["path"].as_str().unwrap()).is_file(),
                "产物路径应存在: {a}"
            );
        }
        // 真实执行落盘
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "wait e2e payload");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn wait_true_failed_pipeline_reports_failed_status() {
        let _guard = execution::lock_for_tests();
        execution::clear_registry_for_tests();

        let root = unique_root("wait-fail");
        write_pipe_file(&root, "wait-fail-pipe");
        // 输入文件不存在 → file_input 节点失败 → 任务 failed
        let state = test_state(root.clone());
        let resp = oneshot(
            app(state),
            json_request(
                axum::http::Method::POST,
                "/pipelines/execute",
                json!({
                    "pipeline_id": "wait-fail-pipe",
                    "wait": true,
                    "inputs": {
                        "input": { "path": root.join("missing.txt").display().to_string() },
                        "output": { "path": root.join("never.txt").display().to_string() }
                    }
                }),
            ),
        )
        .await;
        let (status, body) = response_json(resp).await;
        assert_eq!(status, axum::http::StatusCode::OK, "{body}");
        assert_eq!(body["status"], "failed", "{body}");
        assert!(body["artifacts"].as_array().unwrap().is_empty());
    }

    /// 本地 mock 回调端点：捕获 POST body（仅回环，无外部网络）。
    /// 同步 bind 拿到端口后 spawn serve，避免在 async 上下文阻塞等地址。
    async fn spawn_callback_capture() -> (String, Arc<Mutex<Vec<Value>>>) {
        let captured: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let store = captured.clone();
        let app = axum::Router::new().route(
            "/cb",
            axum::routing::post(move |axum::Json(body): axum::Json<Value>| {
                let store = store.clone();
                async move {
                    store.lock().unwrap().push(body);
                    axum::http::StatusCode::OK
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}/cb"), captured)
    }

    async fn poll_captured(captured: &Mutex<Vec<Value>>) -> Option<Value> {
        for _ in 0..200 {
            {
                let v = captured.lock().unwrap();
                if let Some(first) = v.first() {
                    return Some(first.clone());
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        None
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn callback_url_receives_terminal_post() {
        let _guard = execution::lock_for_tests();
        execution::clear_registry_for_tests();

        let (url, captured) = spawn_callback_capture().await;

        let root = unique_root("cb-http");
        write_pipe_file(&root, "cb-pipe");
        let src = root.join("cb-src.txt");
        let dest = root.join("cb-out.txt");
        write_file(&src, b"callback e2e");

        let state = test_state(root.clone());
        let resp = oneshot(
            app(state),
            json_request(
                axum::http::Method::POST,
                "/pipelines/execute",
                json!({
                    "pipeline_id": "cb-pipe",
                    "wait": false,
                    "callback_url": url,
                    "inputs": {
                        "input": { "path": src.display().to_string() },
                        "output": { "path": dest.display().to_string() }
                    }
                }),
            ),
        )
        .await;
        let (status, body) = response_json(resp).await;
        assert_eq!(status, axum::http::StatusCode::ACCEPTED, "{body}");
        let task_id = body["task_id"].as_str().unwrap().to_string();

        let record = wait_terminal(&task_id).await.expect("任务应终结");
        assert_eq!(record.status, execution::TaskState::Completed);

        // 回调异步投递 → 轮询捕获
        let cb = poll_captured(&captured).await.expect("回调应送达 mock 端点");
        assert_eq!(cb["task_id"], task_id);
        assert_eq!(cb["status"], "completed");
        let artifacts = cb["artifacts"].as_array().expect("回调 artifacts");
        assert_eq!(artifacts.len(), 2);
        assert!(artifacts.iter().all(|a| a["size"].as_u64().unwrap_or(0) > 0));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn unreachable_callback_does_not_block_task() {
        let _guard = execution::lock_for_tests();
        execution::clear_registry_for_tests();

        let root = unique_root("cb-dead");
        write_pipe_file(&root, "cb-dead-pipe");
        let src = root.join("cbd-src.txt");
        let dest = root.join("cbd-out.txt");
        write_file(&src, b"callback best-effort");

        let state = test_state(root.clone());
        let resp = oneshot(
            app(state),
            json_request(
                axum::http::Method::POST,
                "/pipelines/execute",
                json!({
                    "pipeline_id": "cb-dead-pipe",
                    "wait": true,
                    // 回环丢弃端口：连接必然被拒 → best-effort 仅 warn
                    "callback_url": "http://127.0.0.1:9/cb",
                    "inputs": {
                        "input": { "path": src.display().to_string() },
                        "output": { "path": dest.display().to_string() }
                    }
                }),
            ),
        )
        .await;
        let (status, body) = response_json(resp).await;
        assert_eq!(status, axum::http::StatusCode::OK, "{body}");
        assert_eq!(body["status"], "completed", "回调失败不影响任务终态: {body}");
    }
}

// ─── 4. VRAM 预算 fixture 验证（§6.3）────────────────────────────────────────

mod e2e_vram_budget {
    //! 任务书条目 4：vram-budget 端点场景化断言（分层峰值 / over /
    //! unassigned / allow_overcommit=false 透传）。ep-core compute_budget
    //! 的纯计算边界（over 临界、未知估算跳过、环检测）由 ep-core 内联测试
    //! 覆盖；本模块经全路由树验证端点接线 + 配置透传。

    use serde_json::json;

    use ep_core::config::AppConfig;
    use ep_core::module::discovery::{DiscoveredModule, DiscoveryStatus};
    use ep_core::port::PortManager;
    use ep_core::types::{ComputeBackend, ComputeDevice, DeviceId};

    use crate::common::*;
    use crate::state::AppState;

    /// fixture manifest：variant small=2048（变体级）/ large 无变体级 →
    /// 模块级兜底 4096（A6 数据源语义）
    fn vram_manifest() -> ep_core::module::manifest::ModuleManifest {
        toml::from_str(
            r#"
[module]
id = "e2e-asr"
name = "VRAM E2E"
version = "0.1.0"
description = "test"
category = "asr"
genre = "test"
license = "MIT"

[runtime]
type = "python"

[compute]
backends = ["cuda"]
vram_estimate_mb = 4096

[interface]
type = "http"

[[models]]
id = "small"
name = "Small"
source = "huggingface"
target_dir = "e2e-asr-small"
vram_estimate_mb = 2048

[[models]]
id = "large"
name = "Large"
source = "huggingface"
target_dir = "e2e-asr-large"
default = true
"#,
        )
        .unwrap()
    }

    fn gpu(idx: u32, total: Option<u32>, used: Option<u32>) -> ComputeDevice {
        ComputeDevice {
            id: DeviceId::Cuda(idx),
            backend: ComputeBackend::Cuda,
            name: format!("E2E GPU {idx}"),
            total_memory_mb: total,
            used_memory_mb: used,
            utilization: None,
            temperature: None,
        }
    }

    fn vram_state(
        allow_overcommit: bool,
        devices: Vec<ComputeDevice>,
    ) -> std::sync::Arc<AppState> {
        let root = unique_root("vram");
        let mut config = AppConfig::default();
        config.compute.allow_overcommit = allow_overcommit;
        let module = DiscoveredModule {
            manifest: Some(vram_manifest()),
            path: root.join("modules").join("e2e-asr"),
            status: DiscoveryStatus::Valid,
        };
        std::sync::Arc::new(AppState::new(
            root,
            config,
            devices,
            vec![module],
            PortManager::new(18000, 19000),
        ))
    }

    /// 分层峰值：layer0 = a(3000)+b(2000) 并行，layer1 = c(4000) → 峰值 5000
    #[tokio::test]
    async fn layered_peak_over_and_unassigned_via_endpoint() {
        let state = vram_state(true, vec![gpu(0, Some(6000), Some(500))]);
        let body = json!({
            "spec": {
                "nodes": [
                    { "id": "a", "kind": "module", "module_id": "e2e-asr",
                      "model": "ep.x.e2e-asr@small", "device": "cuda:0",
                      "params": {}, "label": "" },
                    { "id": "b", "kind": "module", "module_id": "e2e-asr",
                      "model": "ep.x.e2e-asr@large", "device": "auto" },
                    { "id": "c", "kind": "module", "module_id": "e2e-asr",
                      "device": "cuda:0" }
                ],
                "edges": [
                    { "from": ["a", "output"], "to": ["c", "input"] },
                    { "from": ["b", "output"], "to": ["c", "input"] }
                ]
            }
        });
        let resp = oneshot(
            app(state),
            json_request(axum::http::Method::POST, "/pipelines/vram-budget", body),
        )
        .await;
        let (status, v) = response_json(resp).await;
        assert_eq!(status, axum::http::StatusCode::OK, "{v}");

        // cuda:0 账本：layer0 只有 a=2048（small pin）；layer1 c=4096（模块级兜底）
        // → 峰值 4096
        let d = &v["devices"][0];
        assert_eq!(d["device_id"], "cuda:0");
        assert_eq!(d["pipeline_mb"], 4096, "跨层取峰值: {v}");
        assert_eq!(d["items"][0]["node_id"], "c");
        // used 500 + 4096 > 6000? 否 → over=false
        assert_eq!(d["over"], false);

        // auto 节点 b（large 无变体级 → 4096 兜底）入未分配池
        assert_eq!(v["unassigned_mb"], 4096);
        assert_eq!(v["unassigned"][0]["node_id"], "b");
        assert_eq!(v["allow_overcommit"], true);
    }

    /// ep-core `compute_budget` 纯计算层 fixture 直验：分层峰值 / over /
    /// unassigned / allow_overcommit=false 语义（与端点接线互为印证）。
    /// 场景：三层 DAG（in → a+b 并行 → c），a/b 绑 cuda:0、c auto。
    #[test]
    fn compute_budget_layered_peak_over_unassigned_fixture() {
        use ep_core::pipeline::vram::{compute_budget, DeviceCapacity, VramNodeEstimate};

        let nodes = vec![
            VramNodeEstimate { node_id: "in".into(), device: "auto".into(), vram_mb: None },
            VramNodeEstimate { node_id: "a".into(), device: "cuda:0".into(), vram_mb: Some(3000) },
            VramNodeEstimate { node_id: "b".into(), device: "cuda:0".into(), vram_mb: Some(2000) },
            VramNodeEstimate { node_id: "c".into(), device: "auto".into(), vram_mb: Some(4000) },
        ];
        let edges = vec![
            ("in".to_string(), "a".to_string()),
            ("in".to_string(), "b".to_string()),
            ("a".to_string(), "c".to_string()),
            ("b".to_string(), "c".to_string()),
        ];
        let devices = vec![DeviceCapacity {
            device_id: "cuda:0".into(),
            total_mb: Some(6000),
            used_mb: Some(1500),
        }];

        // allow_overcommit=false：报告原样携带（放行策略由执行层决定）
        let report = compute_budget(&nodes, &edges, &devices, false).unwrap();
        let d = &report.devices[0];
        // layer1 = a+b = 5000 为 cuda:0 峰值（layer2 只有 auto 节点 c）
        assert_eq!(d.pipeline_mb, 5000, "分层峰值 = 并行层求和");
        assert_eq!(d.items.len(), 2);
        // 1500 + 5000 > 6000 → over
        assert!(d.over, "used + pipeline > total → over");
        // auto 节点 c 入未分配池（layer2 峰值 4000）
        assert_eq!(report.unassigned_mb, 4000);
        assert_eq!(report.unassigned[0].node_id, "c");
        assert!(!report.allow_overcommit);

        // 同一场景 allow_overcommit=true 仅翻转报告标志（峰值/over 计算不变）
        let report = compute_budget(&nodes, &edges, &devices, true).unwrap();
        assert!(report.allow_overcommit);
        assert_eq!(report.devices[0].pipeline_mb, 5000);
        assert!(report.devices[0].over);
    }

    /// over=true 场景 + allow_overcommit=false 透传（放行策略由执行层消费）
    #[tokio::test]
    async fn over_budget_with_allow_overcommit_false() {
        let state = vram_state(false, vec![gpu(0, Some(3000), Some(1500))]);
        let body = json!({
            "spec": {
                "nodes": [
                    { "id": "a", "kind": "module", "module_id": "e2e-asr",
                      "model": "ep.x.e2e-asr@small", "device": "cuda:0" }
                ],
                "edges": []
            }
        });
        let resp = oneshot(
            app(state),
            json_request(axum::http::Method::POST, "/pipelines/vram-budget", body),
        )
        .await;
        let (status, v) = response_json(resp).await;
        assert_eq!(status, axum::http::StatusCode::OK, "{v}");
        assert_eq!(v["devices"][0]["over"], true, "1500+2048 > 3000: {v}");
        assert_eq!(
            v["allow_overcommit"], false,
            "config.compute.allow_overcommit=false 应透传进报告"
        );
    }
}

// ─── 5. 闸门与取消 E2E（§6.8 / P1-11）───────────────────────────────────────

mod e2e_gate_cancel {
    //! 任务书条目 5：max_parallel=1 两任务并发 → 一个 queued（队列位置经
    //! GET /pipelines/{id}/tasks 可见）→ 首个完成后续运行；取消排队任务
    //! 绝不执行。
    // 测试锁跨 await 串行化（同 e2e_direct_exec 注释）
    #![allow(clippy::await_holding_lock)]

    use serde_json::json;

    use crate::api::execute::execution;
    use crate::common::*;

    fn write_pipe(root: &std::path::Path, id: &str, src: &std::path::Path, dest: &std::path::Path) {
        write_file(
            &root.join("config").join("pipelines").join(format!("{id}.toml")),
            copy_pipeline_toml(id, src, dest).as_bytes(),
        );
    }

    async fn submit_by_id(app: axum::Router, pipeline_id: &str) -> String {
        let resp = oneshot(
            app,
            json_request(
                axum::http::Method::POST,
                "/pipelines/execute",
                json!({ "pipeline_id": pipeline_id }),
            ),
        )
        .await;
        let (status, body) = response_json(resp).await;
        assert_eq!(status, axum::http::StatusCode::ACCEPTED, "{body}");
        body["task_id"].as_str().unwrap().to_string()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn max_parallel_one_queues_then_runs_after_first_completes() {
        let _guard = execution::lock_for_tests();
        execution::clear_registry_for_tests();

        let root = unique_root("gate");
        let src_a = root.join("g-a-src.txt");
        let dest_a = root.join("g-a-out.txt");
        let src_b = root.join("g-b-src.txt");
        let dest_b = root.join("g-b-out.txt");
        write_file(&src_a, b"gate A");
        write_file(&src_b, b"gate B");
        write_pipe(&root, "gate-a", &src_a, &dest_a);
        write_pipe(&root, "gate-b", &src_b, &dest_b);

        let state = test_state(root.clone());
        state.config.write().await.pipeline.max_parallel = 1;
        // 持闸钩子：首个任务阻塞在引擎执行前，占住全局闸门
        execution::set_test_run_hook_for_pipelines_test();

        let task_a = submit_by_id(app(state.clone()), "gate-a").await;
        // A 提升为 running（闸门计数在 try_promote 内同步增加）
        wait_status(&task_a, execution::TaskState::Running)
            .await
            .expect("A 应进入 running");

        let task_b = submit_by_id(app(state.clone()), "gate-b").await;
        // B 排队：queued + 队列位置 1
        let b_rec = wait_status(&task_b, execution::TaskState::Queued)
            .await
            .expect("B 应排队");
        assert_eq!(b_rec.queue_position, Some(1), "队列位置从 1 起");

        // 队列位置经管线任务视图可见（§6.8 排队可见性）
        let resp = oneshot(
            app(state.clone()),
            get_request("/pipelines/gate-b/tasks"),
        )
        .await;
        let (status, body) = response_json(resp).await;
        assert_eq!(status, axum::http::StatusCode::OK);
        let list = body.as_array().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0]["status"], "queued");
        assert_eq!(list[0]["queue_position"], 1, "{body}");

        // 放闸：A 完成 → 准入 B → B 完成
        execution::release_test_run_hook_for_pipelines_test();
        let a_rec = wait_terminal(&task_a).await.expect("A 应终结");
        assert_eq!(a_rec.status, execution::TaskState::Completed);
        let b_rec = wait_terminal(&task_b).await.expect("B 应终结");
        assert_eq!(
            b_rec.status,
            execution::TaskState::Completed,
            "首个完成后续任务应运行: {:?}",
            b_rec.error
        );
        assert_eq!(std::fs::read_to_string(&dest_a).unwrap(), "gate A");
        assert_eq!(std::fs::read_to_string(&dest_b).unwrap(), "gate B");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn cancel_queued_task_never_executes() {
        let _guard = execution::lock_for_tests();
        execution::clear_registry_for_tests();

        let root = unique_root("cancel-q");
        let src_a = root.join("cq-a-src.txt");
        let dest_a = root.join("cq-a-out.txt");
        let src_b = root.join("cq-b-src.txt");
        let dest_b = root.join("cq-b-out.txt");
        write_file(&src_a, b"cancel A");
        write_file(&src_b, b"cancel B");
        write_pipe(&root, "cq-a", &src_a, &dest_a);
        write_pipe(&root, "cq-b", &src_b, &dest_b);

        let state = test_state(root.clone());
        state.config.write().await.pipeline.max_parallel = 1;
        execution::set_test_run_hook_for_pipelines_test();

        let task_a = submit_by_id(app(state.clone()), "cq-a").await;
        wait_status(&task_a, execution::TaskState::Running)
            .await
            .expect("A 应进入 running");
        let task_b = submit_by_id(app(state.clone()), "cq-b").await;
        wait_status(&task_b, execution::TaskState::Queued)
            .await
            .expect("B 应排队");

        // 取消排队任务 → 200 + cancelled
        let resp = oneshot(
            app(state.clone()),
            json_request(
                axum::http::Method::POST,
                &format!("/tasks/{task_b}/cancel"),
                json!({}),
            ),
        )
        .await;
        let (status, body) = response_json(resp).await;
        assert_eq!(status, axum::http::StatusCode::OK, "{body}");
        assert_eq!(body["status"], "cancelled");
        let b_rec = execution::snapshot(&task_b).expect("B 记录应在");
        assert_eq!(b_rec.status, execution::TaskState::Cancelled);

        // 放闸：A 正常完成；B 绝不执行
        execution::release_test_run_hook_for_pipelines_test();
        let a_rec = wait_terminal(&task_a).await.expect("A 应终结");
        assert_eq!(a_rec.status, execution::TaskState::Completed);
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        let b_rec = execution::snapshot(&task_b).expect("B 记录应在");
        assert_eq!(
            b_rec.status,
            execution::TaskState::Cancelled,
            "放闸后 B 仍应为 cancelled（绝不执行）"
        );
        assert!(
            b_rec.started_running_at.is_none(),
            "排队中取消的任务从未进入 running"
        );
        assert!(!dest_b.exists(), "B 的输出文件不应产生");

        // 重复取消终态任务 → 409
        let resp = oneshot(
            app(state),
            json_request(
                axum::http::Method::POST,
                &format!("/tasks/{task_b}/cancel"),
                json!({}),
            ),
        )
        .await;
        let (status, _) = response_json(resp).await;
        assert_eq!(status, axum::http::StatusCode::CONFLICT);
    }
}

// ─── 6. video-to-srt 条件回归（§15.1 Linux 已验证流程）──────────────────────

mod e2e_video_to_srt {
    //! 任务书条目 6：本机**无 ffmpeg 无模块 venv** → 检测缺失即打印原因跳过。
    //! 测试体按 §15.1 Linux 侧已验证流程编写（ffmpeg 提取音频 → faster-whisper
    //! transcribe → SRT），供具备 ffmpeg + faster-whisper venv + 模型的
    //! 真实环境（Linux 验证机 / Wave 5）运行。
    //!
    //! 运行时注意：条件满足时以**仓库根**为 AppState root（模块/venv/模型
    //! 资产所在），任务产物落仓库 workspace/tasks —— 与真实 daemon 运行一致。
    // 测试锁跨 await 串行化（同 e2e_direct_exec 注释）
    #![allow(clippy::await_holding_lock)]

    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use serde_json::json;

    use ep_core::config::AppConfig;
    use ep_core::module::discovery::discover_modules;
    use ep_core::port::PortManager;

    use crate::api::execute::execution;
    use crate::common::*;
    use crate::state::AppState;

    fn ffmpeg_available() -> bool {
        std::process::Command::new("ffmpeg")
            .arg("-version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// venv python 路径（平台分支：Windows Scripts/python.exe | Linux bin/python）
    fn venv_python(repo_root: &Path) -> PathBuf {
        let venv = repo_root
            .join("runtime")
            .join("venvs")
            .join("faster-whisper");
        if cfg!(target_os = "windows") {
            venv.join("Scripts").join("python.exe")
        } else {
            venv.join("bin").join("python")
        }
    }

    fn repo_root() -> PathBuf {
        // crates/ep-daemon → 仓库根（worktree 内同样成立）
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .components()
            .collect()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn video_to_srt_full_regression_conditional() {
        let repo = repo_root();
        let module_manifest_path = repo
            .join("modules")
            .join("faster-whisper")
            .join("module.toml");

        // ── 条件检测（任一缺失 → 打印原因跳过）─────────────────────────
        if !ffmpeg_available() {
            eprintln!("SKIP video-to-srt: ffmpeg 不在 PATH（本机无 ffmpeg）");
            return;
        }
        if !module_manifest_path.is_file() {
            eprintln!(
                "SKIP video-to-srt: 模块缺失 {}",
                module_manifest_path.display()
            );
            return;
        }
        if !venv_python(&repo).is_file() {
            eprintln!(
                "SKIP video-to-srt: faster-whisper venv 缺失 {}",
                venv_python(&repo).display()
            );
            return;
        }

        let _guard = execution::lock_for_tests();
        execution::clear_registry_for_tests();

        let mut config = AppConfig::default();
        config.resolve_paths(&repo);
        let modules = discover_modules(&repo.join("modules"));

        // 默认模型就绪检查（缺失 → 跳过而非失败：模型资产不入 git）
        let manifest = modules
            .iter()
            .find(|m| {
                m.manifest
                    .as_ref()
                    .map(|mf| mf.module.id == "faster-whisper")
                    .unwrap_or(false)
            })
            .and_then(|m| m.manifest.clone());
        let Some(manifest) = manifest else {
            eprintln!("SKIP video-to-srt: faster-whisper manifest 无效");
            return;
        };
        let mgr = ep_core::model::ModelManager::new(&config.models, &repo);
        let statuses = mgr.check_model_status("faster-whisper", &manifest);
        let default_ready = manifest
            .models
            .iter()
            .find(|m| m.default)
            .or(manifest.models.first())
            .map(|m| {
                matches!(
                    statuses.get(&m.id),
                    Some(ep_core::model::ModelStatus::Ready)
                )
            })
            .unwrap_or(false);
        if !default_ready {
            eprintln!("SKIP video-to-srt: faster-whisper 默认模型未就绪（models 缓存缺失）");
            return;
        }

        // ── 生成测试视频（lavfi：2s 图块 + 440Hz 正弦音轨）──────────────
        let work = unique_root("v2s");
        let video = work.join("input.avi");
        let gen = std::process::Command::new("ffmpeg")
            .args([
                "-f", "lavfi", "-i", "testsrc=duration=2:size=64x64:rate=5",
                "-f", "lavfi", "-i", "sine=frequency=440:duration=2",
                "-c:v", "mpeg4", "-c:a", "pcm_s16le", "-shortest", "-y",
            ])
            .arg(&video)
            .output()
            .expect("ffmpeg 生成测试视频");
        assert!(gen.status.success(), "ffmpeg 生成失败: {:?}", gen);
        assert!(video.is_file());

        // ── 真实执行：/api/pipelines/execute wait:true ─────────────────
        let devices = ep_core::compute::detect_all_devices(&config.compute.disabled_backends);
        let state = std::sync::Arc::new(AppState::new(
            repo.clone(),
            config,
            devices,
            modules,
            PortManager::new(18000, 19000),
        ));
        let resp = oneshot(
            app(state.clone()),
            json_request(
                axum::http::Method::POST,
                "/pipelines/execute",
                json!({
                    "pipeline_id": "video-to-srt",
                    "wait": true,
                    "inputs": {
                        "input": { "path": video.display().to_string() }
                    }
                }),
            ),
        )
        .await;
        let (status, body) = response_json(resp).await;
        assert_eq!(status, axum::http::StatusCode::OK, "{body}");
        assert_eq!(body["status"], "completed", "video-to-srt 全链: {body}");

        // SRT 产物存在（正弦音轨无语音 → 内容可能为空，不断言文本）
        let artifacts = body["artifacts"].as_array().unwrap();
        let srt = artifacts
            .iter()
            .find(|a| a["path"].as_str().unwrap_or("").ends_with(".srt"))
            .expect("output 节点应产出 .srt");
        assert!(Path::new(srt["path"].as_str().unwrap()).is_file());

        // 任务注册表终态与产物一致（仲裁 #40 产物透传面）
        let record = execution::snapshot(body["task_id"].as_str().unwrap())
            .expect("任务记录");
        assert!(!record.artifacts.is_empty());
        tokio::time::sleep(Duration::from_millis(100)).await;

        // teardown：回收自动拉起的模块子进程（faster-whisper adapter 及其
        // 进程树）——进程内 harness 不走 main.rs 优雅退出路径，不显式停止
        // 会留下孤儿 adapter 监听 0.0.0.0:18000，污染后续测试/开发运行
        stop_all_running_modules(&state).await;
    }
}

// ─── 7. harness teardown 回收：进程内 E2E 必须显式停止模块子进程 ────────────

mod e2e_harness_teardown {
    //! 进程内 `Router::oneshot` harness 不经过 main.rs 优雅退出路径，
    //! `common::stop_all_running_modules` 是 E2E 的唯一回收手段。本模块用
    //! 真实长活子进程验证该路径：拉起 → 运行中 → teardown 停止
    //! （stop_module 内含 Windows cmd /C 进程树回收）→ 无残留实例、端口释放。

    use std::collections::HashMap;

    use ep_core::module::manifest::{
        ComputeConfig, InterfaceConfig, InterfaceType, ModuleInfo, ModuleManifest, RuntimeConfig,
        RuntimeType,
    };
    use ep_core::types::{ComputeBackend, DeviceId, ModuleCategory, ServiceStatus};

    use crate::common::*;

    /// 长活 native 模块 manifest（keepalive 命令，形状同 main.rs 优雅退出
    /// 测试的 shutdown fixture）
    fn teardown_test_manifest() -> ModuleManifest {
        ModuleManifest {
            module: ModuleInfo {
                id: "td-mod".into(),
                name: "teardown-test".into(),
                version: "0.1.0".into(),
                description: String::new(),
                category: ModuleCategory::Other("test".into()),
                genre: String::new(),
                authors: vec![],
                license: None,
                homepage: None,
                tags: vec![],
            },
            runtime: RuntimeConfig {
                requirements_by_backend: Default::default(),
                runtime_type: RuntimeType::Native,
                python_version: None,
                requirements: None,
                entrypoint: None,
                start_command: Some(keepalive_command()),
                binaries: None,
            },
            compute: ComputeConfig {
                backends: vec![ComputeBackend::Cpu],
                default_backend: Some(ComputeBackend::Cpu),
                vram_estimate_mb: None,
                min_vram_mb: None,
                env: None,
            },
            models: vec![],
            interface: InterfaceConfig {
                interface_type: InterfaceType::Http,
                health_endpoint: Some("/health".into()),
                ready_timeout_secs: Some(60),
                working_dir: None,
                capabilities: vec![],
            },
        }
    }

    // teardown 停止运行中的真实子进程并释放端口（无残留）
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn teardown_stops_running_module_and_releases_port() {
        let root = unique_root("teardown");
        let state = test_state(root);

        // 拉起真实长活子进程（keepalive：Windows ping / Unix sleep）
        {
            let mut pm = state.process_manager.write().await;
            pm.start_module(
                "td-mod",
                &teardown_test_manifest(),
                DeviceId::Cpu,
                39100,
                HashMap::new(),
            )
            .await
            .expect("keepalive module should start");
        }
        {
            let pm = state.process_manager.read().await;
            assert_eq!(pm.list_running().len(), 1, "子进程应在运行中");
        }

        stop_all_running_modules(&state).await;

        // 后置：实例停止、无运行残留、端口已释放
        {
            let pm = state.process_manager.read().await;
            assert!(pm.list_running().is_empty());
            assert_eq!(pm.get_status("td-mod"), Some(&ServiceStatus::Stopped));
        }
        assert!(
            state.port_manager.read().await.get_port("td-mod").is_none(),
            "teardown 应释放端口"
        );
    }
}
