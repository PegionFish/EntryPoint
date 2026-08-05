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
}
