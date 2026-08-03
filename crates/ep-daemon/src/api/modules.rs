use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
    Json,
};
use serde::Serialize;
use serde_json::{Value, json};
use tracing::{info, warn};

use ep_core::model::{ModelManager, ModelStatus};
use ep_core::module::discovery::{DiscoveredModule, DiscoveryStatus};
use ep_core::types::{DeviceId, ServiceStatus};

use super::err_response;
use crate::state::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/modules", get(list_modules))
        .route("/modules/{id}/start", post(start_module))
        .route("/modules/{id}/stop", post(stop_module))
        .route("/modules/{id}/status", get(module_status))
        .route("/modules/{id}/logs", get(module_logs))
}

// ─── Response types ─────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub(crate) struct ModuleResponse {
    id: String,
    name: String,
    version: String,
    description: String,
    category: String,
    path: String,
    status: String,
    service_status: String,
}

// ─── Helpers ────────────────────────────────────────────────────────────────

/// 服务状态 → 规范小写串（供 list_modules / module_status 共用）
pub(crate) fn status_str(status: &ServiceStatus) -> &'static str {
    match status {
        ServiceStatus::NotReady => "not_ready",
        ServiceStatus::Stopped => "stopped",
        ServiceStatus::Preparing => "preparing",
        ServiceStatus::Starting => "starting",
        ServiceStatus::Running => "running",
        ServiceStatus::Error(_) => "error",
    }
}

/// 按 module_id 查找已发现的模块
async fn find_module(state: &AppState, id: &str) -> Option<DiscoveredModule> {
    let modules = state.modules.read().await;
    modules
        .iter()
        .find(|m| {
            m.manifest
                .as_ref()
                .map(|mf| mf.module.id == id)
                .unwrap_or(false)
        })
        .cloned()
}

// ─── Handlers ───────────────────────────────────────────────────────────────

pub async fn list_modules(
    State(state): State<Arc<AppState>>,
) -> Json<Vec<ModuleResponse>> {
    let modules = state.modules.read().await;
    let pm = state.process_manager.read().await;

    let resp: Vec<ModuleResponse> = modules
        .iter()
        .map(|m| {
            let (id, name, version, description, category) =
                if let Some(ref manifest) = m.manifest {
                    (
                        manifest.module.id.clone(),
                        manifest.module.name.clone(),
                        manifest.module.version.clone(),
                        manifest.module.description.clone(),
                        manifest.module.category.to_string(),
                    )
                } else {
                    let dir_name = m
                        .path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    (
                        dir_name.clone(),
                        dir_name.clone(),
                        String::new(),
                        String::new(),
                        String::new(),
                    )
                };

            let discovery_status = match &m.status {
                DiscoveryStatus::Valid => "valid".to_string(),
                DiscoveryStatus::Invalid(reason) => format!("invalid: {reason}"),
            };

            // 规范小写状态串（不再使用 Rust Debug 格式）
            let service_status = pm
                .get_status(&id)
                .map(status_str)
                .unwrap_or("stopped")
                .to_string();

            ModuleResponse {
                id,
                name,
                version,
                description,
                category,
                path: m.path.display().to_string(),
                status: discovery_status,
                service_status,
            }
        })
        .collect();

    Json(resp)
}

/// POST /api/modules/:id/start
///
/// 错误码语义：404 模块不存在 / 409 状态冲突或模型未就绪 / 500 内部错误。
pub async fn start_module(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> (StatusCode, Json<Value>) {
    // 1. 模块必须存在
    let module = match find_module(&state, &id).await {
        Some(m) => m,
        None => {
            return err_response(
                &state,
                StatusCode::NOT_FOUND,
                "apiCore.module.notFound",
                &[("id", id)],
            )
            .await
        }
    };

    let manifest = match module.manifest {
        Some(mf) => mf,
        None => {
            return err_response(
                &state,
                StatusCode::INTERNAL_SERVER_ERROR,
                "apiCore.module.invalidManifest",
                &[("id", id)],
            )
            .await
        }
    };

    // 2. 状态冲突检查：已在运行/启动中/准备中 → 409
    {
        let pm = state.process_manager.read().await;
        if let Some(s) = pm.get_status(&id) {
            match s {
                ServiceStatus::Running | ServiceStatus::Starting | ServiceStatus::Preparing => {
                    return err_response(
                        &state,
                        StatusCode::CONFLICT,
                        "apiCore.module.alreadyRunningWithStatus",
                        &[("status", status_str(s).to_string())],
                    )
                    .await;
                }
                _ => {}
            }
        }
    }

    // 3. 模型前置检查：default 模型缺失 → 409（依赖检查本期不做）
    if !manifest.models.is_empty() {
        let mgr = {
            let config = state.config.read().await;
            ModelManager::new(&config.models, &state.root)
        };
        let statuses = mgr.check_model_status(&id, &manifest);
        if let Some(model) = manifest
            .models
            .iter()
            .find(|m| m.default)
            .or(manifest.models.first())
        {
            if matches!(statuses.get(&model.id), Some(ModelStatus::Missing)) {
                return err_response(
                    &state,
                    StatusCode::CONFLICT,
                    "apiCore.module.modelNotReady",
                    &[("model", model.name.clone())],
                )
                .await;
            }
        }
    }

    // 4. 分配端口
    let port = {
        let mut pm = state.port_manager.write().await;
        match pm.allocate(&id) {
            Ok(p) => p,
            Err(e) => {
                return err_response(
                    &state,
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "apiCore.module.portAllocationFailed",
                    &[("detail", e.to_string())],
                )
                .await
            }
        }
    };

    // 5. 选择设备：manifest 声明的后端优先，否则回退 CPU
    let device = {
        let devices = state.devices.read().await;
        devices
            .iter()
            .find(|d| manifest.compute.backends.contains(&d.backend))
            .map(|d| d.id.clone())
            .unwrap_or(DeviceId::Cpu)
    };

    // 6. 构建环境变量
    let env_vars = {
        let root = &state.root;
        let module_dir = &module.path;
        let model_dir = if let Some(model) = manifest.models.iter().find(|m| m.default) {
            root.join("models").join(&model.target_dir)
        } else if let Some(model) = manifest.models.first() {
            root.join("models").join(&model.target_dir)
        } else {
            module_dir.clone()
        };

        let mut vars = HashMap::new();
        vars.insert("ROOT".to_string(), root.to_string_lossy().to_string());
        vars.insert("MODULE_DIR".to_string(), module_dir.to_string_lossy().to_string());
        vars.insert("MODEL_DIR".to_string(), model_dir.to_string_lossy().to_string());
        vars.insert("PORT".to_string(), port.to_string());
        vars.insert("DEVICE".to_string(), device.to_string());
        vars.insert("BACKEND".to_string(), device.backend().to_string());
        vars.insert(
            "DEVICE_INDEX".to_string(),
            device.index().map(|i| i.to_string()).unwrap_or_default(),
        );
        vars.insert("WORKSPACE".to_string(), root.join("workspace").to_string_lossy().to_string());
        vars
    };

    info!(module_id = %id, %port, %device, "starting module");

    // 7. 启动模块进程
    let mut pm = state.process_manager.write().await;
    match pm
        .start_module(&id, &manifest, device, port, env_vars)
        .await
    {
        Ok(()) => (
            StatusCode::OK,
            Json(json!({
                "status": "starting",
                "module_id": id,
                "port": port
            })),
        ),
        Err(e) => {
            warn!(module_id = %id, error = %e, "failed to start module");
            // 启动失败：释放端口
            state.port_manager.write().await.release(&id);
            // "already running/starting" 属状态冲突，其余为内部错误
            if e.to_string().contains("already running") {
                err_response(
                    &state,
                    StatusCode::CONFLICT,
                    "apiCore.module.alreadyRunning",
                    &[("id", id)],
                )
                .await
            } else {
                err_response(
                    &state,
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "apiCore.module.startFailed",
                    &[("detail", e.to_string())],
                )
                .await
            }
        }
    }
}

/// POST /api/modules/:id/stop
pub async fn stop_module(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> (StatusCode, Json<Value>) {
    // 模块必须已被发现
    if find_module(&state, &id).await.is_none() {
        return err_response(
            &state,
            StatusCode::NOT_FOUND,
            "apiCore.module.notFound",
            &[("id", id)],
        )
        .await;
    }

    let mut pm = state.process_manager.write().await;
    if pm.get_instance(&id).is_none() {
        return err_response(
            &state,
            StatusCode::NOT_FOUND,
            "apiCore.module.notRunning",
            &[("id", id)],
        )
        .await;
    }

    match pm.stop_module(&id).await {
        Ok(()) => {
            drop(pm);
            state.port_manager.write().await.release(&id);
            info!(module_id = %id, "module stopped");
            (
                StatusCode::OK,
                Json(json!({
                    "status": "stopped",
                    "module_id": id
                })),
            )
        }
        Err(e) => {
            err_response(
                &state,
                StatusCode::INTERNAL_SERVER_ERROR,
                "apiCore.module.stopFailed",
                &[("detail", e.to_string())],
            )
            .await
        }
    }
}

/// GET /api/modules/:id/status
pub async fn module_status(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> (StatusCode, Json<Value>) {
    if find_module(&state, &id).await.is_none() {
        return err_response(
            &state,
            StatusCode::NOT_FOUND,
            "apiCore.module.notFound",
            &[("id", id)],
        )
        .await;
    }

    let pm = state.process_manager.read().await;
    match pm.get_instance(&id) {
        Some(inst) => {
            let uptime_secs = inst
                .started_at
                .map(|t| {
                    let now_ts = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(0);
                    (now_ts - t.timestamp()).max(0)
                })
                .unwrap_or(0);

            (
                StatusCode::OK,
                Json(json!({
                    "module_id": id,
                    "status": status_str(&inst.status),
                    "port": inst.port,
                    "uptime_secs": uptime_secs
                })),
            )
        }
        None => (
            StatusCode::OK,
            Json(json!({
                "module_id": id,
                "status": "stopped",
                "port": null,
                "uptime_secs": 0
            })),
        ),
    }
}

/// GET /api/modules/:id/logs
///
/// 模块不存在 → 404；模块存在但未启动 → 200 + 空行列表。
pub async fn module_logs(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> (StatusCode, Json<Value>) {
    if find_module(&state, &id).await.is_none() {
        return err_response(
            &state,
            StatusCode::NOT_FOUND,
            "apiCore.module.notFound",
            &[("id", id)],
        )
        .await;
    }

    let pm = state.process_manager.read().await;
    match pm.get_instance(&id) {
        Some(inst) => {
            let lines: Vec<&String> = inst.log_buffer.iter().collect();
            (
                StatusCode::OK,
                Json(json!({
                    "module_id": id,
                    "lines": lines
                })),
            )
        }
        None => (
            StatusCode::OK,
            Json(json!({
                "module_id": id,
                "lines": []
            })),
        ),
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    /// 构造指定语言的测试 AppState（空模块表，tempdir root）
    fn test_state(language: &str) -> Arc<AppState> {
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        let mut config = ep_core::config::AppConfig::default();
        config.general.language = language.to_string();
        Arc::new(AppState::new(
            std::env::temp_dir().join(format!("ep-api-modules-test-{}-{seq}", std::process::id())),
            config,
            vec![],
            vec![],
            ep_core::port::PortManager::new(18000, 19000),
        ))
    }

    #[test]
    fn status_str_all_variants_are_canonical_lowercase() {
        assert_eq!(status_str(&ServiceStatus::NotReady), "not_ready");
        assert_eq!(status_str(&ServiceStatus::Stopped), "stopped");
        assert_eq!(status_str(&ServiceStatus::Preparing), "preparing");
        assert_eq!(status_str(&ServiceStatus::Starting), "starting");
        assert_eq!(status_str(&ServiceStatus::Running), "running");
        assert_eq!(
            status_str(&ServiceStatus::Error("boom".into())),
            "error"
        );
    }

    // 默认语言 zh-CN：错误文案与迁移前完全一致
    #[tokio::test]
    async fn start_unknown_module_error_zh_cn() {
        let state = test_state("zh-CN");
        let (status, body) = start_module(State(state), Path("ghost".to_string())).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body.0["error"], "模块不存在：ghost");
    }

    // 同一请求在 config.language=en 时返回英文错误
    #[tokio::test]
    async fn start_unknown_module_error_en() {
        let state = test_state("en");
        let (status, body) = start_module(State(state), Path("ghost".to_string())).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body.0["error"], "Module not found: ghost");
    }

    // stop 同一请求双语对照
    #[tokio::test]
    async fn stop_unknown_module_error_zh_cn_and_en() {
        let state = test_state("zh-CN");
        let (status, body) = stop_module(State(state), Path("ghost".to_string())).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body.0["error"], "模块不存在：ghost");

        let state = test_state("en");
        let (status, body) = stop_module(State(state), Path("ghost".to_string())).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body.0["error"], "Module not found: ghost");
    }

    // logs 同一请求双语对照
    #[tokio::test]
    async fn logs_unknown_module_error_zh_cn_and_en() {
        let state = test_state("zh-CN");
        let (status, body) = module_logs(State(state), Path("ghost".to_string())).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body.0["error"], "模块不存在：ghost");

        let state = test_state("en");
        let (status, body) = module_logs(State(state), Path("ghost".to_string())).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body.0["error"], "Module not found: ghost");
    }

    // status 同一请求双语对照
    #[tokio::test]
    async fn status_unknown_module_error_zh_cn_and_en() {
        let state = test_state("zh-CN");
        let (status, body) = module_status(State(state), Path("ghost".to_string())).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body.0["error"], "模块不存在：ghost");

        let state = test_state("en");
        let (status, body) = module_status(State(state), Path("ghost".to_string())).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body.0["error"], "Module not found: ghost");
    }
}
