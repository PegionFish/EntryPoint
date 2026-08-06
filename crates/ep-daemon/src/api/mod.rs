pub mod autostart;
pub mod config;
pub mod deps;
pub mod devices;
pub mod execute;
pub mod health;
pub mod models;
pub mod modules;
pub mod packs;
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
/// Wave S 骨架：packs 路由已预注册（§8.1 共 7 条），stub handler 统一返回
/// 501 + i18n `common.tip.comingSoon`（{"error":"功能即将上线"}），
/// 接管代理与契约见 `packs.rs` 文件头注释。
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
        .merge(packs::router())
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

/// 模块启动路径的设备选择（D-4 调度器接线）——手动启动（modules.rs）与
/// 自动拉起（autostart.rs）共用：经 ep-core 共享选择核心统一分配
/// （manifest backends 兼容过滤 + `[compute].disabled_backends` 剔除 +
/// 策略/VRAM 闸门）；调度器拒绝且无兼容设备时保留旧 first-match 时代的
/// Cpu 兜底语义。config 快照先行取出再取 devices 读锁，避免与设备刷新
/// 写锁交叉等待。
pub(crate) async fn select_module_device(
    state: &AppState,
    manifest: &ep_core::module::manifest::ModuleManifest,
) -> ep_core::types::DeviceId {
    let (vram_mb, strategy, allow_overcommit, disabled) = {
        let config = state.config.read().await;
        (
            ep_core::compute::scheduler::module_vram_request(&config, manifest),
            ep_core::compute::scheduler::scheduling_strategy_for(&config),
            config.compute.allow_overcommit,
            config.compute.disabled_backends.clone(),
        )
    };
    let devices = state.devices.read().await;
    ep_core::compute::scheduler::select_device_for_module(
        &devices,
        manifest,
        vram_mb,
        strategy,
        allow_overcommit,
        &disabled,
    )
    .unwrap_or(ep_core::types::DeviceId::Cpu)
}

/// 模块 venv python 解释器路径（双平台口径与 ep-core [`ep_core::env::EnvManager::venv_python_path`]
/// 一致：Windows `runtime/venvs/<id>/Scripts/python.exe`、其他平台 `bin/python`）
pub(crate) fn module_venv_python_path(
    root: &std::path::Path,
    module_id: &str,
) -> std::path::PathBuf {
    let venv_dir = root.join("runtime").join("venvs").join(module_id);
    if cfg!(target_os = "windows") {
        venv_dir.join("Scripts").join("python.exe")
    } else {
        venv_dir.join("bin").join("python")
    }
}

/// venv 就绪门禁（任务 #10）：手动启动（modules.rs）、自动拉起（autostart.rs）、
/// 模型下载（models.rs / packs.rs）四条路径共用的唯一入口。
///
/// 先经 ep-core [`ep_core::env::EnvManager::is_venv_ready`] 判定：python 存在且
/// 无 requirements 即就绪；否则要求 `.ep_deps_hash` 与依赖栈哈希匹配。修复
/// 旧门禁仅看 python.exe 存在性导致"半壳 venv"（只有解释器、未装依赖）误判
/// 就绪的问题；未就绪才调 `ensure_venv` 准备。非 Python 运行时直接返回
/// 常规路径（no-op）。成功返回 venv python 路径；失败返回英文技术细节，
/// 调用方经 i18n 键 `apiModels.venvPrepFailed` 生成用户文案。
pub(crate) async fn ensure_module_venv_ready(
    state: &Arc<AppState>,
    module_id: &str,
    manifest: &ep_core::module::manifest::ModuleManifest,
) -> Result<std::path::PathBuf, String> {
    use ep_core::module::manifest::RuntimeType;
    use tracing::info;

    if manifest.runtime.runtime_type != RuntimeType::Python {
        return Ok(module_venv_python_path(&state.root, module_id));
    }
    let (python_cfg, network_cfg) = {
        let cfg = state.config.read().await;
        (cfg.python.clone(), cfg.network.clone())
    };
    let root = state.root.clone();
    let mid = module_id.to_string();
    let py_ver = manifest.runtime.python_version.clone().unwrap_or_default();
    let req_rel = manifest
        .runtime
        .requirements
        .clone()
        .unwrap_or_else(|| "requirements.txt".to_string());

    let prep = tokio::task::spawn_blocking(move || {
        let env_mgr =
            ep_core::env::EnvManager::new(&root, &python_cfg).with_network(&network_cfg);
        let req_path = root.join("modules").join(&mid).join(&req_rel);
        if env_mgr.is_venv_ready(&mid, &req_path) {
            return Ok(env_mgr.venv_python_path(&mid));
        }
        info!(
            module_id = %mid,
            "venv not ready (missing or deps hash mismatch), preparing Python environment"
        );
        env_mgr.ensure_venv(&mid, &py_ver, &req_path)
    })
    .await;

    match prep {
        Ok(Ok(path)) => Ok(path),
        Ok(Err(e)) => Err(format!("{e:#}")),
        Err(e) => Err(format!("venv prep task panicked: {e}")),
    }
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

    // ── D-4：select_module_device（启动路径设备选择共享助手） ──────────────

    fn manifest_with_backends(
        id: &str,
        backends: &[ep_core::types::ComputeBackend],
    ) -> ep_core::module::manifest::ModuleManifest {
        let backends_str = backends
            .iter()
            .map(|b| format!("\"{b}\""))
            .collect::<Vec<_>>()
            .join(", ");
        toml::from_str(&format!(
            r#"
[module]
id = "{id}"
name = "t"
version = "0.1.0"
description = "t"
category = "asr"
genre = "test"

[runtime]
type = "native"
binaries = {{ "x" = "x" }}

[compute]
backends = [{backends_str}]

[interface]
type = "http"
"#
        ))
        .unwrap()
    }

    fn state_with_devices(
        config: ep_core::config::AppConfig,
        devices: Vec<ep_core::types::ComputeDevice>,
    ) -> Arc<AppState> {
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        Arc::new(AppState::new(
            std::env::temp_dir().join(format!("ep-api-dev-test-{}-{seq}", std::process::id())),
            config,
            devices,
            vec![],
            ep_core::port::PortManager::new(18000, 19000),
        ))
    }

    fn cuda_device(index: u32, total_mb: u32) -> ep_core::types::ComputeDevice {
        ep_core::types::ComputeDevice {
            id: ep_core::types::DeviceId::Cuda(index),
            backend: ep_core::types::ComputeBackend::Cuda,
            name: format!("GPU-{index}"),
            total_memory_mb: Some(total_mb),
            used_memory_mb: None,
            utilization: None,
            temperature: None,
        }
    }

    fn cpu_device() -> ep_core::types::ComputeDevice {
        ep_core::types::ComputeDevice {
            id: ep_core::types::DeviceId::Cpu,
            backend: ep_core::types::ComputeBackend::Cpu,
            name: "CPU".to_string(),
            total_memory_mb: None,
            used_memory_mb: None,
            utilization: None,
            temperature: None,
        }
    }

    // 加速后端优先：cuda+cpu 声明 + cuda/cpu 设备 → 选中 cuda:0（替代旧 first-match 盲选）
    #[tokio::test]
    async fn select_module_device_prefers_accelerator() {
        use ep_core::types::{ComputeBackend, DeviceId};
        let state = state_with_devices(
            ep_core::config::AppConfig::default(),
            vec![cuda_device(0, 8192), cpu_device()],
        );
        let mf = manifest_with_backends("mod-a", &[ComputeBackend::Cuda, ComputeBackend::Cpu]);
        assert_eq!(
            select_module_device(&state, &mf).await,
            DeviceId::Cuda(0)
        );
    }

    // disabled_backends 全局过滤：禁用 cuda → CPU 保底
    #[tokio::test]
    async fn select_module_device_respects_disabled_backends() {
        use ep_core::types::{ComputeBackend, DeviceId};
        let mut config = ep_core::config::AppConfig::default();
        config.compute.disabled_backends = vec![ComputeBackend::Cuda];
        let state = state_with_devices(config, vec![cuda_device(0, 8192), cpu_device()]);
        let mf = manifest_with_backends("mod-a", &[ComputeBackend::Cuda, ComputeBackend::Cpu]);
        assert_eq!(select_module_device(&state, &mf).await, DeviceId::Cpu);
    }

    // 无兼容设备 → 保留旧 unwrap_or(Cpu) 兜底语义
    #[tokio::test]
    async fn select_module_device_cpu_fallback_when_no_compatible() {
        use ep_core::types::{ComputeBackend, DeviceId};
        let state = state_with_devices(
            ep_core::config::AppConfig::default(),
            vec![cpu_device()],
        );
        let mf = manifest_with_backends("mod-a", &[ComputeBackend::Rocm]);
        assert_eq!(select_module_device(&state, &mf).await, DeviceId::Cpu);
    }

    // VRAM 闸门：超限且未开超分 → 声明 cpu 则 CPU 保底（旧 first-match 无此能力）
    #[tokio::test]
    async fn select_module_device_vram_gate_cpu_fallback() {
        use ep_core::types::DeviceId;
        let mut config = ep_core::config::AppConfig::default();
        config.compute.allow_overcommit = false;
        let state = state_with_devices(config, vec![cuda_device(0, 512), cpu_device()]);
        let mf: ep_core::module::manifest::ModuleManifest = toml::from_str(
            r#"
[module]
id = "mod-a"
name = "t"
version = "0.1.0"
description = "t"
category = "asr"
genre = "test"

[runtime]
type = "native"
binaries = { "x" = "x" }

[compute]
backends = ["cuda", "cpu"]
vram_estimate_mb = 8000

[interface]
type = "http"
"#,
        )
        .unwrap();
        assert_eq!(select_module_device(&state, &mf).await, DeviceId::Cpu);
    }

    // ── 任务 #10：ensure_module_venv_ready（venv 就绪门禁共享助手） ────────

    fn python_manifest_toml(id: &str) -> String {
        format!(
            r#"
[module]
id = "{id}"
name = "t"
version = "0.1.0"
description = "t"
category = "asr"
genre = "test"

[runtime]
type = "python"

[compute]
backends = ["cpu"]

[interface]
type = "http"
"#
        )
    }

    fn state_at(root: std::path::PathBuf) -> Arc<AppState> {
        Arc::new(AppState::new(
            root,
            ep_core::config::AppConfig::default(),
            vec![],
            vec![],
            ep_core::port::PortManager::new(18000, 19000),
        ))
    }

    fn unique_venv_root(tag: &str) -> std::path::PathBuf {
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "ep-api-venv-{tag}-{}-{seq}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    // 非 Python 运行时 → no-op，直接返回常规路径（不触发任何准备）
    #[tokio::test]
    async fn ensure_module_venv_ready_non_python_noop() {
        let root = unique_venv_root("native");
        let state = state_at(root.clone());
        let mf = manifest_with_backends("native-mod", &[ep_core::types::ComputeBackend::Cpu]);
        let path = ensure_module_venv_ready(&state, "native-mod", &mf)
            .await
            .expect("非 Python 运行时应直接返回");
        assert_eq!(path, module_venv_python_path(&root, "native-mod"));
        let _ = std::fs::remove_dir_all(&root);
    }

    // 就绪 venv（哈希匹配）→ 直接返回解释器路径，不重跑准备
    #[tokio::test]
    async fn ensure_module_venv_ready_ready_venv_returns_python() {
        let root = unique_venv_root("ready");
        let state = state_at(root.clone());
        let mf: ep_core::module::manifest::ModuleManifest =
            toml::from_str(&python_manifest_toml("ready-mod")).unwrap();

        // 预置假 python + requirements + 匹配哈希 → is_venv_ready 命中
        let py = module_venv_python_path(&root, "ready-mod");
        std::fs::create_dir_all(py.parent().unwrap()).unwrap();
        std::fs::write(&py, b"fake").unwrap();
        let req = root.join("modules/ready-mod/requirements.txt");
        std::fs::create_dir_all(req.parent().unwrap()).unwrap();
        std::fs::write(&req, "fastapi>=0.100.0\n").unwrap();
        let hash = ep_core::env::compute_deps_hash(&req, None).unwrap();
        std::fs::write(
            root.join("runtime/venvs/ready-mod/.ep_deps_hash"),
            &hash,
        )
        .unwrap();

        let path = ensure_module_venv_ready(&state, "ready-mod", &mf)
            .await
            .expect("哈希匹配的 venv 应判就绪");
        assert_eq!(path, py);
        let _ = std::fs::remove_dir_all(&root);
    }

    // 半壳 venv（只有假解释器、有 requirements、无哈希）→ 门禁判未就绪 →
    // 触发 ensure_venv；假解释器无法承载 uv pip install（或宿主无 uv）→ 确定性失败
    #[tokio::test]
    async fn ensure_module_venv_ready_half_shell_triggers_prep_failure() {
        let root = unique_venv_root("halfshell");
        let state = state_at(root.clone());
        let mf: ep_core::module::manifest::ModuleManifest =
            toml::from_str(&python_manifest_toml("half-mod")).unwrap();

        let py = module_venv_python_path(&root, "half-mod");
        std::fs::create_dir_all(py.parent().unwrap()).unwrap();
        std::fs::write(&py, b"fake").unwrap();
        let req = root.join("modules/half-mod/requirements.txt");
        std::fs::create_dir_all(req.parent().unwrap()).unwrap();
        std::fs::write(&req, "ep-halfshell-nonexistent-pkg==1.0\n").unwrap();

        let err = ensure_module_venv_ready(&state, "half-mod", &mf)
            .await
            .expect_err("半壳 venv 必须触发准备并失败");
        assert!(!err.is_empty(), "失败必须携带技术细节");
        let _ = std::fs::remove_dir_all(&root);
    }
}
