use std::sync::Arc;

use axum::{
    Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post, put},
    Json,
};
use serde::Serialize;
use serde_json::{Value, json};
use tracing::{info, warn};

use ep_core::model::{ModelManager, ModelStatus, active_model_for};
use ep_core::module::discovery::{DiscoveredModule, DiscoveryStatus};
use ep_core::module::manifest::CapabilityDecl;
use ep_core::types::ServiceStatus;

use super::err_response;
use crate::state::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/modules", get(list_modules))
        .route("/modules/{id}/start", post(start_module))
        .route("/modules/{id}/stop", post(stop_module))
        .route("/modules/{id}/status", get(module_status))
        .route("/modules/{id}/logs", get(module_logs))
        // §8.1 激活变体切换（版本单槽位 §5.2）：路径前缀 /models 与模型 API 对齐，
        // handler 归属 modules.rs（模块激活状态写入 config.active_models）
        .route(
            "/models/{module_id}/{model_id}/variant",
            put(set_model_variant),
        )
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
    /// 模块 manifest 声明的能力列表（§8.2，CapabilityDecl 逐字段原样序列化）。
    /// P0-1 根治：前端据此数据驱动渲染能力/参数表单，不再硬编码 capability 映射。
    capabilities: Vec<CapabilityDecl>,
    /// 当前绑定设备（如 "cuda:0"；未运行为 null）—— P2-4 设备列的真实数据源（§8.2）
    device: Option<String>,
    /// manifest 声明的计算后端（D-Device：前端算力源下拉的兼容性过滤依据）
    backends: Vec<String>,
    /// 解析后的激活变体 id（门禁 #33：config.active_models → default → 首变体；
    /// 无模型模块为 null）—— 前端统一页"激活变体"投影的权威数据源
    active_model_id: Option<String>,
}

// ─── Helpers ────────────────────────────────────────────────────────────────

// 激活变体声明解析已上提为 api/mod.rs 的公共助手 [`super::active_model_decl`]
// （F1 修复：手动启动与自动拉起预检共用唯一真源，杜绝口径漂移）。

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
    let cfg = state.config.read().await;

    let resp: Vec<ModuleResponse> = modules
        .iter()
        .map(|m| {
            let (id, name, version, description, category, capabilities) =
                if let Some(ref manifest) = m.manifest {
                    (
                        manifest.module.id.clone(),
                        manifest.module.name.clone(),
                        manifest.module.version.clone(),
                        manifest.module.description.clone(),
                        manifest.module.category.to_string(),
                        manifest.interface.capabilities.clone(),
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
                        Vec::new(),
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

            // 当前绑定设备（运行中实例携带；未运行为 None → JSON null，P2-4）
            let device = pm
                .get_instance(&id)
                .and_then(|inst| inst.device.as_ref().map(|d| d.to_string()));

            // 激活变体（门禁 #33）：三级回退解析结果透传，前端不再启发式推断
            let active_model_id = m
                .manifest
                .as_ref()
                .and_then(|manifest| active_model_for(&cfg, manifest).map(|s| s.to_string()));

            ModuleResponse {
                id,
                name,
                version,
                description,
                category,
                path: m.path.display().to_string(),
                status: discovery_status,
                service_status,
                capabilities,
                device,
                backends: m
                    .manifest
                    .as_ref()
                    .map(|mf| {
                        mf.compute
                            .backends
                            .iter()
                            .map(|b| b.to_string())
                            .collect()
                    })
                    .unwrap_or_default(),
                active_model_id,
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

    // 3. 模型前置检查：激活变体缺失 → 409（§5.2 单槽位：经 active_model_for
    //    三级回退选取，不再"死板取 default"）
    if !manifest.models.is_empty() {
        let (mgr, active) = {
            let config = state.config.read().await;
            let active = super::active_model_decl(&config, &manifest)
                .map(|m| (m.id.clone(), m.name.clone()));
            (ModelManager::new(&config.models, &state.root), active)
        };
        let statuses = mgr.check_model_status(&id, &manifest);
        if let Some((active_id, active_name)) = active {
            if matches!(statuses.get(&active_id), Some(ModelStatus::Missing)) {
                return err_response(
                    &state,
                    StatusCode::CONFLICT,
                    "apiCore.module.modelNotReady",
                    &[("model", active_name)],
                )
                .await;
            }
        }
    }

    // 3.5 venv 就绪门禁（任务 #10 + HETERO_DIST_PLAN M2/M3 接线）：手动启动此前
    //     完全不做 venv 准备，与自动拉起（autostart.rs）路径不一致；现统一走共享
    //     助手（is_venv_ready 哈希门禁，修复"半壳 venv"误判；未就绪才 ensure_venv）。
    //     设备选择前移至此处：分后端 venv 依赖本次分配设备所属 backend
    //     （依赖文件 requirements_by_backend 解析 + <module>--<backend> 目录）。
    let device = super::select_module_device(&state, &manifest).await;
    if let Err(detail) =
        super::ensure_module_venv_ready(&state, &id, &manifest, Some(device.backend())).await
    {
        warn!(module_id = %id, error = %detail, "venv prep failed before start");
        return err_response(
            &state,
            StatusCode::INTERNAL_SERVER_ERROR,
            "apiModels.venvPrepFailed",
            &[("detail", detail)],
        )
        .await;
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

    // 5. 设备已选择（见 3.5 前移说明，D-4 调度器接线语义不变）

    // 6. 构建环境变量（缺陷 #4 残余修复）：统一委托 ep-core 公共构建函数
    //    （手动启动/自动拉起/独立模式/桌面端同一真源），注入 MODELS_ROOT
    //    （装配为 EP_MODELS_ROOT，adapter 在 params.model 覆盖非激活变体时
    //    据此解析变体子目录本地权重），消除旧手写 env 的键集漂移。
    let env_vars = super::module_start_env_vars(&state, &id, &manifest, &device, port).await;

    info!(module_id = %id, %port, %device, "starting module");

    // 7. 启动模块进程
    let mut pm = state.process_manager.write().await;
    match pm
        .start_module(&id, &manifest, device, port, env_vars)
        .await
    {
        Ok(()) => {
            // 空闲自动释放时间基准：手动启动即视为活跃起点
            {
                let now = chrono::Utc::now().timestamp();
                state
                    .module_activity
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .insert(id.clone(), now);
            }
            (
                StatusCode::OK,
                Json(json!({
                    "status": "starting",
                    "module_id": id,
                    "port": port
                })),
            )
        }
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

/// PUT /api/models/:module_id/:model_id/variant — 切换模块激活变体（§5.2/§8.1）。
///
/// 版本单槽位语义：写 `config.active_models[module_id] = model_id` 并落盘
/// config/app.toml（复用 [`AppConfig::save`] 路径）。响应形状对齐前端
/// `ModelVariantResponse`：
/// - `needs_download`：目标变体本地非 Ready（Missing/Incomplete/Importable）；
/// - `needs_restart`：模块正在运行（running/starting/preparing）→ 重启后生效。
///
/// 错误：400 请求体缺 model_id 或与路径不一致；404 模块/变体不存在；
/// 500 清单无效或配置落盘失败。
pub async fn set_model_variant(
    State(state): State<Arc<AppState>>,
    Path((module_id, model_id)): Path<(String, String)>,
    body: Option<Json<Value>>,
) -> (StatusCode, Json<Value>) {
    // 0. 请求体：必带 model_id 且与路径 {model_id} 一致（§8.1 请求 `{model_id}`）
    let body = body.map(|Json(v)| v).unwrap_or_else(|| json!({}));
    let body_model_id = match body.get("model_id").and_then(|v| v.as_str()) {
        Some(id) if !id.trim().is_empty() => id.to_string(),
        _ => {
            return err_response(&state, StatusCode::BAD_REQUEST, "apiModels.missingModelId", &[])
                .await
        }
    };
    if body_model_id != model_id {
        return err_response(
            &state,
            StatusCode::BAD_REQUEST,
            "apiModels.variantMismatch",
            &[
                ("path_id", model_id),
                ("body_id", body_model_id),
            ],
        )
        .await;
    }

    // 1. 模块必须存在且 manifest 声明了目标变体
    let module = match find_module(&state, &module_id).await {
        Some(m) => m,
        None => {
            return err_response(
                &state,
                StatusCode::NOT_FOUND,
                "apiCore.module.notFound",
                &[("id", module_id)],
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
                &[("id", module_id)],
            )
            .await
        }
    };
    if !manifest.models.iter().any(|m| m.id == model_id) {
        return err_response(
            &state,
            StatusCode::NOT_FOUND,
            "apiModels.modelNotFound",
            &[
                ("model_id", model_id),
                ("module_id", module_id),
            ],
        )
        .await;
    }

    // 2. 写 config.active_models + 落盘（单槽位 §5.2）。
    //    错误分支必须先释放写锁再调 err_response（其内部经 state.lang() 读 config，
    //    持写锁调用会在 tokio RwLock 上死锁）——与 put_config 同款约束。
    let config_dir = state.root.join("config");
    let save_result = {
        let mut config = state.config.write().await;
        config.active_models.insert(module_id.clone(), model_id.clone());
        config.save(&config_dir)
    };
    if let Err(e) = save_result {
        tracing::error!(error = %e, "failed to persist active_models config");
        return err_response(
            &state,
            StatusCode::INTERNAL_SERVER_ERROR,
            "apiCore.config.saveFailed",
            &[("detail", e.to_string())],
        )
        .await;
    }

    // 3. 下载检查：目标变体非 Ready → needs_download
    let needs_download = {
        let config = state.config.read().await;
        let mgr = ModelManager::new(&config.models, &state.root);
        let statuses = mgr.check_model_status(&module_id, &manifest);
        !matches!(statuses.get(&model_id), Some(ModelStatus::Ready))
    };

    // 4. 重启提示：模块运行中（running/starting/preparing）→ needs_restart
    let needs_restart = {
        let pm = state.process_manager.read().await;
        matches!(
            pm.get_status(&module_id),
            Some(
                ServiceStatus::Running | ServiceStatus::Starting | ServiceStatus::Preparing
            )
        )
    };

    info!(
        module_id = %module_id,
        model_id = %model_id,
        needs_download = %needs_download,
        needs_restart = %needs_restart,
        "active model variant switched"
    );

    (
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "needs_download": needs_download,
            "needs_restart": needs_restart
        })),
    )
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ep_core::config::AppConfig;
    use ep_core::module::manifest::{ModelDecl, ModuleManifest};
    use ep_core::types::DeviceId;
    use std::collections::HashMap;
    use std::path::PathBuf;
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

    // ── 任务 #10 回归：手动启动触发 venv 准备（此前手动路径完全不做 venv 准备）
    //    半壳 venv（假解释器 + requirements 无哈希）→ 500 + venvPrepFailed 文案，
    //    失败先于端口分配 → 无端口泄漏
    #[tokio::test]
    async fn start_module_half_shell_venv_triggers_prep_and_500() {
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "ep-api-modules-half-{}-{seq}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();

        let manifest: ModuleManifest = toml::from_str(
            r#"
[module]
id = "half-start-mod"
name = "半壳手动启动测试"
version = "0.1.0"
description = "half-shell manual start regression"
category = "asr"
genre = "test"

[runtime]
type = "python"

[compute]
backends = ["cpu"]

[interface]
type = "http"
"#,
        )
        .unwrap();
        let module = DiscoveredModule {
            path: root.join("modules/half-start-mod"),
            manifest: Some(manifest),
            status: DiscoveryStatus::Valid,
        };
        let state = Arc::new(AppState::new(
            root.clone(),
            AppConfig::default(),
            vec![],
            vec![module],
            ep_core::port::PortManager::new(18000, 19000),
        ));

        // 预置半壳 venv：假 python + requirements（无 .ep_deps_hash）
        let py = crate::api::module_venv_python_path(&root, "half-start-mod");
        std::fs::create_dir_all(py.parent().unwrap()).unwrap();
        std::fs::write(&py, b"fake").unwrap();
        let req = root.join("modules/half-start-mod/requirements.txt");
        std::fs::create_dir_all(req.parent().unwrap()).unwrap();
        std::fs::write(&req, "ep-halfshell-nonexistent-pkg==1.0\n").unwrap();

        let (status, body) = start_module(
            State(state.clone()),
            Path("half-start-mod".to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(
            body.0["error"]
                .as_str()
                .unwrap()
                .contains("Python 环境准备失败"),
            "应为 venvPrepFailed 文案: {}",
            body.0["error"]
        );
        // 门禁先于端口分配：无端口泄漏、无进程实例
        assert!(state
            .port_manager
            .read()
            .await
            .get_port("half-start-mod")
            .is_none());
        assert!(state
            .process_manager
            .read()
            .await
            .get_instance("half-start-mod")
            .is_none());
        let _ = std::fs::remove_dir_all(&root);
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

    // ─── B5：capabilities 数据驱动（P0-1）/ 变体端点（§5.2/§8.1）/ cache_dir（P2-9）───

    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use ep_core::module::manifest::{
        ComputeConfig as ManifestComputeConfig, InterfaceConfig, InterfaceType, ModelSource,
        ModuleInfo, ParamSchema, RuntimeConfig, RuntimeType,
    };
    use ep_core::types::{ComputeBackend, DataType, ModuleCategory};

    fn variant_decl(id: &str, target_dir: &str, default: bool) -> ModelDecl {
        ModelDecl {
            id: id.to_string(),
            name: format!("Demo {id}"),
            source: ModelSource::Huggingface,
            repo_id: Some(format!("org/{target_dir}")),
            url: None,
            target_dir: target_dir.to_string(),
            revision: None,
            size_estimate_mb: None,
            qualified_id: None,
            vram_estimate_mb: None,
            default,
            mirrors: vec![],
        }
    }

    /// manifest fixture：双变体（small=default / large）+ 带完整参数 schema 的能力
    fn fixture_manifest(start_command: Option<&str>) -> ModuleManifest {
        let mut params = HashMap::new();
        params.insert(
            "beam_size".to_string(),
            ParamSchema {
                param_type: "integer".to_string(),
                default: Some(json!(5)),
                description: Some("Beam width".to_string()),
                min: Some(1.0),
                max: Some(20.0),
                step: Some(1.0),
                enum_values: None,
                options: None,
            },
        );
        params.insert(
            "language".to_string(),
            ParamSchema {
                param_type: "string".to_string(),
                default: Some(json!("auto")),
                description: None,
                min: None,
                max: None,
                step: None,
                enum_values: Some(vec!["auto".into(), "zh".into(), "en".into()]),
                options: Some(vec!["auto".into(), "zh".into(), "en".into()]),
            },
        );

        ModuleManifest {
            module: ModuleInfo {
                id: "demo-asr".to_string(),
                name: "Demo ASR".to_string(),
                version: "1.0.0".to_string(),
                description: "fixture ASR module".to_string(),
                category: ModuleCategory::Asr,
                genre: "whisper".to_string(),
                authors: vec![],
                license: None,
                homepage: None,
                tags: vec![],
            },
            runtime: RuntimeConfig {
                runtime_type: RuntimeType::Native,
                python_version: None,
                requirements: None,
                entrypoint: None,
                start_command: start_command.map(str::to_string),
                binaries: None,
                requirements_by_backend: Default::default(),
            },
            compute: ManifestComputeConfig {
                backends: vec![ComputeBackend::Cpu],
                default_backend: None,
                vram_estimate_mb: None,
                min_vram_mb: None,
                env: None,
            },
            models: vec![
                variant_decl("small", "demo-small", true),
                variant_decl("large", "demo-large", false),
            ],
            interface: InterfaceConfig {
                interface_type: InterfaceType::Http,
                health_endpoint: None,
                ready_timeout_secs: None,
                working_dir: None,
                capabilities: vec![CapabilityDecl {
                    name: "transcribe".to_string(),
                    description: "Speech to text".to_string(),
                    input_type: DataType::Audio,
                    output_type: DataType::Json,
                    max_file_size_mb: Some(100),
                    supports_batch: true,
                    params: Some(params),
                }],
            },
        }
    }

    /// tempdir root（唯一）+ 携带一个 fixture 模块的 AppState
    fn module_test_state(config: AppConfig, manifest: ModuleManifest) -> (PathBuf, Arc<AppState>) {
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "ep-api-modules-b5-{}-{seq}",
            std::process::id()
        ));
        let state = Arc::new(AppState::new(
            root.clone(),
            config,
            vec![],
            vec![DiscoveredModule {
                manifest: Some(manifest),
                path: PathBuf::from("modules/demo-asr"),
                status: DiscoveryStatus::Valid,
            }],
            // 独立区间（避开生产默认 18000-19000）：拉起 keepalive 的测试
            // 依赖 Starting/状态机断言，若与并发真实 daemon 的 adapter 端口
            // 重叠，健康探测可能误判就绪（环境性 flake）。
            ep_core::port::PortManager::new(48400, 49000),
        ));
        (root, state)
    }

    fn get_req(uri: &str) -> Request<Body> {
        Request::builder().uri(uri).body(Body::empty()).unwrap()
    }

    fn post_req(uri: &str) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(uri)
            .body(Body::empty())
            .unwrap()
    }

    fn put_json(uri: &str, body: Value) -> Request<Body> {
        Request::builder()
            .method("PUT")
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    async fn collect_json(resp: axum::response::Response) -> Value {
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    // P0-1 根治：GET /modules 原样透传 manifest capabilities（逐字段）
    #[tokio::test]
    async fn list_modules_exposes_full_capabilities_schema() {
        let (_root, state) = module_test_state(AppConfig::default(), fixture_manifest(None));
        let app = super::router().with_state(state);
        let resp = app.oneshot(get_req("/modules")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = collect_json(resp).await;

        let m = &body[0];
        assert_eq!(m["id"], "demo-asr");
        // 未运行 → device 为 null（P2-4 真实数据位，不再"暂不支持"）
        assert!(m["device"].is_null());

        let cap = &m["capabilities"][0];
        assert_eq!(cap["name"], "transcribe");
        assert_eq!(cap["description"], "Speech to text");
        assert_eq!(cap["input_type"], "audio");
        assert_eq!(cap["output_type"], "json");
        assert_eq!(cap["max_file_size_mb"], json!(100));
        assert_eq!(cap["supports_batch"], json!(true));

        // ParamSchema 逐字段（type/default/min/max/step/description/enum/options）
        let beam = &cap["params"]["beam_size"];
        assert_eq!(beam["type"], "integer");
        assert_eq!(beam["default"], json!(5));
        assert_eq!(beam["description"], "Beam width");
        assert_eq!(beam["min"], json!(1.0));
        assert_eq!(beam["max"], json!(20.0));
        assert_eq!(beam["step"], json!(1.0));
        // 缺失字段序列化为 null（CapabilityDecl 原样，不做裁剪）
        assert!(beam["enum"].is_null());
        assert!(beam["options"].is_null());

        let lang = &cap["params"]["language"];
        assert_eq!(lang["type"], "string");
        assert_eq!(lang["default"], "auto");
        assert!(lang["description"].is_null());
        assert_eq!(lang["enum"], json!(["auto", "zh", "en"]));
        assert_eq!(lang["options"], json!(["auto", "zh", "en"]));
    }

    // §5.2/§8.1：变体切换写 config.active_models 并落盘；未下载 → needs_download
    #[tokio::test]
    async fn variant_switch_persists_config_and_flags_needs_download() {
        let (root, state) = module_test_state(AppConfig::default(), fixture_manifest(None));
        let app = super::router().with_state(state.clone());
        let resp = app
            .oneshot(put_json(
                "/models/demo-asr/large/variant",
                json!({"model_id": "large"}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = collect_json(resp).await;
        assert_eq!(body["ok"], json!(true));
        assert_eq!(body["needs_download"], json!(true)); // demo-large 未下载
        assert_eq!(body["needs_restart"], json!(false)); // 模块未运行

        // 配置落盘：config/app.toml [active_models] 单槽位
        let loaded = AppConfig::load(&root.join("config")).unwrap();
        assert_eq!(
            loaded.active_models.get("demo-asr").map(String::as_str),
            Some("large")
        );
        // 内存配置同步
        let cfg = state.config.read().await;
        assert_eq!(
            cfg.active_models.get("demo-asr").map(String::as_str),
            Some("large")
        );
        drop(cfg);
        let _ = std::fs::remove_dir_all(&root);
    }

    // P2-9：变体下载检查走 config.models.cache_dir（自定义目录 Ready → 无需下载）
    #[tokio::test]
    async fn variant_switch_download_check_respects_config_cache_dir() {
        let mut config = AppConfig::default();
        config.models.cache_dir = "custom-models".to_string();
        let (root, state) = module_test_state(config, fixture_manifest(None));

        // 仅预置自定义 cache_dir 下的目标变体（若代码硬编码 root/models 将判 Missing）
        let dir = root.join("custom-models").join("demo-large");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("model.bin"), b"weights").unwrap();

        let app = super::router().with_state(state.clone());
        let resp = app
            .oneshot(put_json(
                "/models/demo-asr/large/variant",
                json!({"model_id": "large"}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = collect_json(resp).await;
        assert_eq!(body["needs_download"], json!(false));

        // 删除自定义缓存目录后 → 同一请求翻转为 needs_download=true
        std::fs::remove_dir_all(root.join("custom-models")).unwrap();
        let app = super::router().with_state(state);
        let resp = app
            .oneshot(put_json(
                "/models/demo-asr/large/variant",
                json!({"model_id": "large"}),
            ))
            .await
            .unwrap();
        let body = collect_json(resp).await;
        assert_eq!(body["needs_download"], json!(true));
        let _ = std::fs::remove_dir_all(&root);
    }

    // 运行中模块：needs_restart=true，且 GET /modules device 列携带真实绑定设备（P2-4）
    #[tokio::test]
    async fn variant_switch_running_module_sets_needs_restart_and_device() {
        // 真实子进程（跨平台空转命令）：spawn 后实例处于 Starting
        let cmd = if cfg!(windows) {
            "ping -n 15 127.0.0.1"
        } else {
            "sleep 15"
        };
        let manifest = fixture_manifest(Some(cmd));
        let (root, state) = module_test_state(AppConfig::default(), manifest.clone());

        // 预置 small → needs_download=false，隔离观察 needs_restart
        let dir = root.join("models").join("demo-small");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("model.bin"), b"weights").unwrap();

        {
            let mut pm = state.process_manager.write().await;
            pm.start_module("demo-asr", &manifest, DeviceId::Cpu, 18123, HashMap::new())
                .await
                .unwrap();
        }

        // 运行中：device 字段携带真实绑定设备（P2-4 数据源）
        let app = super::router().with_state(state.clone());
        let resp = app.oneshot(get_req("/modules")).await.unwrap();
        let body = collect_json(resp).await;
        assert_eq!(body[0]["device"], json!("cpu"));

        // 变体切换 → needs_restart=true
        let app = super::router().with_state(state.clone());
        let resp = app
            .oneshot(put_json(
                "/models/demo-asr/small/variant",
                json!({"model_id": "small"}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = collect_json(resp).await;
        assert_eq!(body["needs_download"], json!(false));
        assert_eq!(body["needs_restart"], json!(true));

        // 清理：终止子进程
        state
            .process_manager
            .write()
            .await
            .stop_module("demo-asr")
            .await
            .unwrap();
        let _ = std::fs::remove_dir_all(&root);
    }

    // 变体端点错误矩阵：400 缺 model_id / 400 路径与请求体不一致 / 404 模块 / 404 变体
    #[tokio::test]
    async fn variant_switch_error_matrix() {
        let (_root, state) = module_test_state(AppConfig::default(), fixture_manifest(None));

        let app = super::router().with_state(state.clone());
        let resp = app
            .oneshot(put_json("/models/demo-asr/large/variant", json!({})))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        let app = super::router().with_state(state.clone());
        let resp = app
            .oneshot(put_json(
                "/models/demo-asr/large/variant",
                json!({"model_id": "small"}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        let app = super::router().with_state(state.clone());
        let resp = app
            .oneshot(put_json("/models/ghost/large/variant", json!({"model_id": "large"})))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        let app = super::router().with_state(state);
        let resp = app
            .oneshot(put_json("/models/demo-asr/huge/variant", json!({"model_id": "huge"})))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    // 启动路径消费 active_models：激活变体（非 default）缺失 → 409 且报激活变体名
    #[tokio::test]
    async fn start_module_model_precheck_uses_active_variant() {
        let mut config = AppConfig::default();
        config
            .active_models
            .insert("demo-asr".into(), "large".into());
        let (root, state) = module_test_state(config, fixture_manifest(None));

        // default 变体 small Ready；激活变体 large Missing
        let small = root.join("models").join("demo-small");
        std::fs::create_dir_all(&small).unwrap();
        std::fs::write(small.join("model.bin"), b"weights").unwrap();

        let app = super::router().with_state(state);
        let resp = app.oneshot(post_req("/modules/demo-asr/start")).await.unwrap();
        // 若仍死板取 default（small 已 Ready）将放行而非 409 —— 证明走 active_model_for
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        let body = collect_json(resp).await;
        let error = body["error"].as_str().unwrap();
        assert!(error.contains("Demo large"), "error: {error}");
        let _ = std::fs::remove_dir_all(&root);
    }

    // P2-9 的 MODEL_DIR 解析矩阵（active 变体选择 × cache_dir × 回退）已随
    // 缺陷 #4 残余修复迁移至 api/mod.rs 的 module_start_env_vars 测试
    // （启动路径 env 统一委托 ep-core build_module_env）。

    // ─── GAP-2：handler 错误路径与状态机边界补测 ──────────────────────

    /// 已发现但未启动的模块 → 404 notRunning（与「模块不存在」的 404 语义区分：
    /// 前者文案携带 notRunning i18n 键，前端据此区分提示）
    #[tokio::test]
    async fn stop_not_running_module_returns_not_running_404() {
        let (_root, state) = module_test_state(AppConfig::default(), fixture_manifest(None));
        let app = super::router().with_state(state);
        let resp = app.oneshot(post_req("/modules/demo-asr/stop")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let body = collect_json(resp).await;
        assert_eq!(body["error"], "模块未在运行：demo-asr");
    }

    /// 未启动模块的状态查询：200 + stopped/port=null/uptime=0（前端轮询契约，
    /// 不是 404——模块存在即有状态）
    #[tokio::test]
    async fn module_status_not_started_reports_stopped_shape() {
        let (_root, state) = module_test_state(AppConfig::default(), fixture_manifest(None));
        let app = super::router().with_state(state);
        let resp = app
            .oneshot(get_req("/modules/demo-asr/status"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = collect_json(resp).await;
        assert_eq!(body["module_id"], "demo-asr");
        assert_eq!(body["status"], "stopped");
        assert!(body["port"].is_null());
        assert_eq!(body["uptime_secs"], 0);
    }

    /// 未启动模块的日志查询：200 + 空行列表（不是 404，同 status 语义）
    #[tokio::test]
    async fn module_logs_not_started_returns_empty_lines() {
        let (_root, state) = module_test_state(AppConfig::default(), fixture_manifest(None));
        let app = super::router().with_state(state);
        let resp = app.oneshot(get_req("/modules/demo-asr/logs")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = collect_json(resp).await;
        assert_eq!(body["module_id"], "demo-asr");
        assert_eq!(body["lines"], json!([]));
    }

    /// 运行中实例的缓冲日志经 handler 可见（log_buffer → lines 数组保序）
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn module_logs_running_instance_returns_buffered_lines() {
        let cmd = if cfg!(windows) {
            "ping -n 15 127.0.0.1"
        } else {
            "sleep 15"
        };
        let manifest = fixture_manifest(Some(cmd));
        let (root, state) = module_test_state(AppConfig::default(), manifest.clone());

        {
            let mut pm = state.process_manager.write().await;
            pm.start_module("demo-asr", &manifest, DeviceId::Cpu, 18125, HashMap::new())
                .await
                .unwrap();
            // 模拟 reader task 回传的日志行（不经宿主环境子进程输出，双平台确定）
            pm.append_log("demo-asr", "ep-log-line-1".to_string());
            pm.append_log("demo-asr", "ep-log-line-2".to_string());
        }

        let app = super::router().with_state(state.clone());
        let resp = app.oneshot(get_req("/modules/demo-asr/logs")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = collect_json(resp).await;
        assert_eq!(body["lines"], json!(["ep-log-line-1", "ep-log-line-2"]));

        // 清理：终止子进程
        state
            .process_manager
            .write()
            .await
            .stop_module("demo-asr")
            .await
            .unwrap();
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 状态机边界：实例处于 Starting（运行中/启动中/准备中同集合）时重复
    /// start → 409 + 携带当前状态串，而非二次拉起进程
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn start_module_state_conflict_409_when_starting() {
        let cmd = if cfg!(windows) {
            "ping -n 15 127.0.0.1"
        } else {
            "sleep 15"
        };
        let manifest = fixture_manifest(Some(cmd));
        let (root, state) = module_test_state(AppConfig::default(), manifest.clone());

        {
            let mut pm = state.process_manager.write().await;
            pm.start_module("demo-asr", &manifest, DeviceId::Cpu, 18126, HashMap::new())
                .await
                .unwrap();
        }

        let app = super::router().with_state(state.clone());
        let resp = app.oneshot(post_req("/modules/demo-asr/start")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        let body = collect_json(resp).await;
        let error = body["error"].as_str().unwrap();
        assert!(error.contains("已在运行或正在启动"), "{error}");
        assert!(error.contains("starting"), "错误应携带当前状态串: {error}");

        // 清理：终止子进程
        state
            .process_manager
            .write()
            .await
            .stop_module("demo-asr")
            .await
            .unwrap();
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 端口区间耗尽 → 500 portAllocationFailed（单端口区间被预占，
    /// 模型预检放行：预置激活变体 small Ready）
    #[tokio::test]
    async fn start_module_port_exhaustion_500() {
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "ep-api-modules-portex-{}-{seq}",
            std::process::id()
        ));
        // 激活变体 small Ready → 越过模型预检，直达端口分配
        let dir = root.join("models").join("demo-small");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("model.bin"), b"weights").unwrap();

        // 单端口区间：先被 occupant 占走 → demo-asr 分配必然耗尽
        let port_manager = ep_core::port::PortManager::new(39910, 39910);
        let state = Arc::new(AppState::new(
            root.clone(),
            AppConfig::default(),
            vec![],
            vec![DiscoveredModule {
                manifest: Some(fixture_manifest(None)),
                path: PathBuf::from("modules/demo-asr"),
                status: DiscoveryStatus::Valid,
            }],
            port_manager,
        ));
        state
            .port_manager
            .write()
            .await
            .allocate("occupant")
            .expect("单端口区间首次分配应成功");

        let app = super::router().with_state(state);
        let resp = app.oneshot(post_req("/modules/demo-asr/start")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = collect_json(resp).await;
        let error = body["error"].as_str().unwrap();
        assert!(error.starts_with("端口分配失败"), "{error}");
        assert!(error.contains("exhausted"), "应透传耗尽细节: {error}");
        let _ = std::fs::remove_dir_all(&root);
    }
}
