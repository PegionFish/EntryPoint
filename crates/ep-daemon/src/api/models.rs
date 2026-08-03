//! 模型管理 API — 状态查询、本地导入、下载、删除、更新检查
//!
//! 下载走 ep-core `execute_download_with_progress`（python 子进程 + 目录大小轮询），
//! 进度写入 `state.downloads` 并经 `state.model_download_tx` 广播到 /ws。

use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{delete, get, post},
    Json,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::{debug, info, warn};

use ep_core::model::{
    DownloadProgress, DownloadState, ModelInfo, ModelManager, ModelStatus,
};
use ep_core::module::manifest::{ModelDecl, ModelSource, ModuleManifest};

use super::err_response;
use crate::state::{AppState, DownloadEntry, WsMessage};

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/models", get(list_models))
        // 静态段 downloads 优先于 {module_id} 通配段（matchit 规则），无冲突
        .route("/models/downloads", get(list_downloads))
        .route("/models/{module_id}", get(module_models))
        .route("/models/{module_id}/import", post(import_model))
        .route("/models/{module_id}/download", post(download_model))
        .route(
            "/models/{module_id}/{model_id}/check-update",
            post(check_model_update),
        )
        .route("/models/{module_id}/{model_id}", delete(delete_model))
}

// ─── Request / Response types ───────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ImportRequest {
    /// 模型 ID（对应 module.toml 中 [[models]].id）
    pub model_id: String,
    /// 本地源路径（包含模型文件的目录）
    pub source_path: String,
}

// ─── Helpers ────────────────────────────────────────────────────────────────

/// 从 AppState 构建 ModelManager
///
/// 链式注册：
/// - `with_manifests`：resolve / available_sources / import 路径解析依赖清单；
/// - `with_network`：更新检查 HTTP 客户端的代理注入。
async fn build_model_manager(state: &AppState) -> ModelManager {
    let config = state.config.read().await;
    let modules = state.modules.read().await;
    let manifests: Vec<ModuleManifest> = modules.iter().filter_map(|m| m.manifest.clone()).collect();
    ModelManager::new(&config.models, &state.root)
        .with_manifests(manifests)
        .with_network(config.network.clone())
}

/// 查找模块的 manifest（通过 module_id）
async fn find_module_manifest(
    state: &AppState,
    module_id: &str,
) -> Option<ModuleManifest> {
    let modules = state.modules.read().await;
    modules
        .iter()
        .find(|m| {
            m.manifest
                .as_ref()
                .map(|mf| mf.module.id == module_id)
                .unwrap_or(false)
        })
        .and_then(|m| m.manifest.clone())
}

/// 查找模块 manifest 中的模型声明；模块或模型不存在时返回 404 i18n 错误
async fn find_model_decl(
    state: &Arc<AppState>,
    module_id: &str,
    model_id: &str,
) -> Result<ModelDecl, (StatusCode, Json<Value>)> {
    let Some(manifest) = find_module_manifest(state, module_id).await else {
        return Err(err_response(
            state,
            StatusCode::NOT_FOUND,
            "apiModels.moduleNotFound",
            &[("module_id", module_id.to_string())],
        )
        .await);
    };
    match manifest.models.iter().find(|m| m.id == model_id).cloned() {
        Some(decl) => Ok(decl),
        None => {
            Err(err_response(
                state,
                StatusCode::NOT_FOUND,
                "apiModels.modelNotFound",
                &[
                    ("model_id", model_id.to_string()),
                    ("module_id", module_id.to_string()),
                ],
            )
            .await)
        }
    }
}

/// state.downloads 键约定：`"{module_id}:{model_id}"`
fn download_key(module_id: &str, model_id: &str) -> String {
    format!("{module_id}:{model_id}")
}

/// 解析配置中的下载源字符串（models.default_source）；非法值返回 None
fn parse_model_source(s: &str) -> Option<ModelSource> {
    match s.trim() {
        "huggingface" => Some(ModelSource::Huggingface),
        "modelscope" => Some(ModelSource::Modelscope),
        "url" => Some(ModelSource::Url),
        _ => None,
    }
}

/// 模块 venv python 解释器路径（与 ep-desktop 一致：
/// Windows 为 `Scripts/python.exe`，其他平台为 `bin/python`）
fn venv_python_path(root: &std::path::Path, module_id: &str) -> PathBuf {
    let venv_dir = root.join("runtime").join("venvs").join(module_id);
    if cfg!(target_os = "windows") {
        venv_dir.join("Scripts").join("python.exe")
    } else {
        venv_dir.join("bin").join("python")
    }
}

/// DownloadState → downloads 表 / WS 消息使用的字符串状态
fn download_state_str(state: &DownloadState) -> &'static str {
    match state {
        DownloadState::Downloading => "downloading",
        DownloadState::Completed => "completed",
        DownloadState::Failed(_) => "failed",
        DownloadState::Cancelled => "cancelled",
    }
}

/// 把一条进度事件同步到 downloads 表并广播 WS 消息（发送失败忽略）
fn relay_download_progress(
    downloads: &std::sync::Mutex<std::collections::HashMap<String, DownloadEntry>>,
    ws_tx: &tokio::sync::broadcast::Sender<WsMessage>,
    key: &str,
    progress: &DownloadProgress,
) {
    let state_str = download_state_str(&progress.state);
    {
        let mut map = downloads.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = map.get_mut(key) {
            entry.percent = progress.percent;
            entry.bytes = progress.bytes;
            entry.state = state_str.to_string();
        }
    }
    let _ = ws_tx.send(WsMessage::ModelDownload {
        module_id: progress.module_id.clone(),
        model_id: progress.model_id.clone(),
        percent: progress.percent,
        state: state_str.to_string(),
        bytes: progress.bytes,
    });
}

// ─── Handlers ───────────────────────────────────────────────────────────────

/// GET /api/models — 列出所有模块的模型状态
async fn list_models(State(state): State<Arc<AppState>>) -> Json<Value> {
    let mgr = build_model_manager(&state).await;
    let modules = state.modules.read().await;

    let mut result: Vec<Value> = Vec::new();

    for module in modules.iter() {
        let manifest = match &module.manifest {
            Some(mf) => mf,
            None => continue,
        };

        let module_id = &manifest.module.id;

        // 跳过没有模型声明的模块
        if manifest.models.is_empty() {
            continue;
        }

        let statuses = mgr.check_model_status(module_id, manifest);

        let models: Vec<Value> = manifest
            .models
            .iter()
            .map(|model| {
                let status = statuses
                    .get(&model.id)
                    .cloned()
                    .unwrap_or(ModelStatus::Missing);
                json!({
                    "model_id": model.id,
                    "name": model.name,
                    "target_dir": model.target_dir,
                    "status": status.to_string(),
                    "source": model.source.as_str(),
                    "size_estimate_mb": model.size_estimate_mb,
                    "available_sources": model.available_sources(),
                })
            })
            .collect();

        result.push(json!({
            "module_id": module_id,
            "module_name": manifest.module.name,
            "models": models,
        }));
    }

    Json(json!({ "modules": result }))
}

/// GET /api/models/:module_id — 获取指定模块的模型详情
async fn module_models(
    State(state): State<Arc<AppState>>,
    Path(module_id): Path<String>,
) -> (StatusCode, Json<Value>) {
    let manifest = match find_module_manifest(&state, &module_id).await {
        Some(mf) => mf,
        None => {
            return err_response(
                &state,
                StatusCode::NOT_FOUND,
                "apiModels.moduleNotFound",
                &[("module_id", module_id.clone())],
            )
            .await;
        }
    };

    if manifest.models.is_empty() {
        let lang = state.lang().await;
        return (
            StatusCode::OK,
            Json(json!({
                "module_id": module_id,
                "models": [],
                "message": ep_core::i18n::t(&lang, "apiModels.noModelDecls", &[])
            })),
        );
    }

    let mgr = build_model_manager(&state).await;
    let infos: Vec<ModelInfo> = mgr.get_model_info(&module_id, &manifest);

    let models: Vec<Value> = infos
        .iter()
        .map(|info| {
            json!({
                "model_id": info.model_id,
                "name": info.name,
                "target_dir": info.target_dir,
                "status": info.status.to_string(),
                "size_bytes": info.size_bytes,
                "file_count": info.file_count,
                "local_cache_path": info.local_cache_path,
                "available_sources": info.available_sources,
            })
        })
        .collect();

    (
        StatusCode::OK,
        Json(json!({
            "module_id": module_id,
            "module_name": manifest.module.name,
            "models": models,
        })),
    )
}

/// POST /api/models/:module_id/import — 从本地路径导入模型
async fn import_model(
    State(state): State<Arc<AppState>>,
    Path(module_id): Path<String>,
    Json(req): Json<ImportRequest>,
) -> (StatusCode, Json<Value>) {
    let manifest = match find_module_manifest(&state, &module_id).await {
        Some(mf) => mf,
        None => {
            return err_response(
                &state,
                StatusCode::NOT_FOUND,
                "apiModels.moduleNotFound",
                &[("module_id", module_id.clone())],
            )
            .await;
        }
    };

    // 验证 model_id 存在于 manifest 中
    let model_decl = match manifest.models.iter().find(|m| m.id == req.model_id) {
        Some(m) => m,
        None => {
            let available: Vec<&str> = manifest.models.iter().map(|m| m.id.as_str()).collect();
            return err_response(
                &state,
                StatusCode::NOT_FOUND,
                "apiModels.importModelNotFound",
                &[
                    ("module_id", module_id.clone()),
                    ("model_id", req.model_id.clone()),
                    ("available", available.join(", ")),
                ],
            )
            .await;
        }
    };

    let source_path = PathBuf::from(&req.source_path);
    if !source_path.is_dir() {
        return err_response(
            &state,
            StatusCode::BAD_REQUEST,
            "apiModels.importSourceInvalid",
            &[("path", req.source_path.clone())],
        )
        .await;
    }

    info!(
        module_id = %module_id,
        model_id = %req.model_id,
        source_path = %req.source_path,
        target_dir = %model_decl.target_dir,
        "API: importing model"
    );

    let mgr = build_model_manager(&state).await;

    // 使用 manifest 中的 target_dir 进行导入
    // import_model 内部使用 model_id 构建路径，这里需要确保 target_dir 正确
    // 先直接复制到正确的 target_dir
    let target_dir = mgr.model_dir(&model_decl.target_dir);

    match tokio::fs::create_dir_all(&target_dir).await {
        Ok(()) => {}
        Err(e) => {
            warn!(error = %e, "failed to create target dir");
            return err_response(
                &state,
                StatusCode::INTERNAL_SERVER_ERROR,
                "apiModels.targetDirCreateFailed",
                &[("detail", e.to_string())],
            )
            .await;
        }
    }

    // 执行异步复制
    match copy_dir_all(&source_path, &target_dir).await {
        Ok((file_count, total_bytes)) => {
            // 写入元数据
            let meta = ep_core::model::ModelMeta {
                module_id: module_id.clone(),
                model_id: req.model_id.clone(),
                source: "local_import".to_string(),
                repo_id: String::new(),
                revision: String::new(),
                downloaded_at: chrono::Utc::now().to_rfc3339(),
                total_size_bytes: total_bytes,
            };
            if let Err(e) = mgr.write_meta(&model_decl.target_dir, &meta) {
                warn!(error = %e, "failed to write model meta after import");
            }

            info!(
                module_id = %module_id,
                model_id = %req.model_id,
                file_count = file_count,
                total_bytes = total_bytes,
                "API: model import completed"
            );

            (
                StatusCode::OK,
                Json(json!({
                    "status": "imported",
                    "module_id": module_id,
                    "model_id": req.model_id,
                    "target_dir": model_decl.target_dir,
                    "file_count": file_count,
                    "total_bytes": total_bytes,
                })),
            )
        }
        Err(e) => {
            warn!(error = %e, "model import failed");
            err_response(
                &state,
                StatusCode::INTERNAL_SERVER_ERROR,
                "apiModels.importFailed",
                &[("detail", e.to_string())],
            )
            .await
        }
    }
}

/// 递归复制目录，返回 (文件数, 总字节数)
async fn copy_dir_all(src: &std::path::Path, dst: &std::path::Path) -> anyhow::Result<(usize, u64)> {
    let mut file_count: usize = 0;
    let mut total_bytes: u64 = 0;

    let mut entries = tokio::fs::read_dir(src).await?;
    while let Some(entry) = entries.next_entry().await? {
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());

        if src_path.is_dir() {
            tokio::fs::create_dir_all(&dst_path).await?;
            let (sub_count, sub_bytes) = Box::pin(copy_dir_all(&src_path, &dst_path)).await?;
            file_count += sub_count;
            total_bytes += sub_bytes;
        } else {
            let size = entry.metadata().await.map(|m| m.len()).unwrap_or(0);
            tokio::fs::copy(&src_path, &dst_path).await?;
            file_count += 1;
            total_bytes += size;
        }
    }

    Ok((file_count, total_bytes))
}

// ─── Wave 2：模型下载 / 删除 / 更新检查（W2-A 实现） ────────────────────────

/// POST /api/models/:module_id/download — 启动模型下载
///
/// 请求体：`{"model_id": "...", "source": "huggingface" | "modelscope" | "url"?}`
/// （source 可选，缺省用模型主 source）。
///
/// 状态码：
/// - 202 已受理（后台任务执行下载，进度经 state.downloads + /ws 广播）
/// - 400 请求体非法 / 下载源不可用；404 模块或模型不存在
/// - 409 正在下载中或模型已存在；412 venv Python 环境未就绪
async fn download_model(
    State(state): State<Arc<AppState>>,
    Path(module_id): Path<String>,
    body: Option<Json<Value>>,
) -> (StatusCode, Json<Value>) {
    // ── 解析请求体 ──
    let body = body.map(|Json(v)| v).unwrap_or_else(|| json!({}));
    let model_id = match body.get("model_id").and_then(|v| v.as_str()) {
        Some(id) if !id.trim().is_empty() => id.to_string(),
        _ => {
            return err_response(
                &state,
                StatusCode::BAD_REQUEST,
                "apiModels.missingModelId",
                &[],
            )
            .await;
        }
    };
    let source: Option<ModelSource> = match body.get("source") {
        None | Some(Value::Null) => None,
        Some(v) => match serde_json::from_value::<ModelSource>(v.clone()) {
            Ok(s) => Some(s),
            Err(_) => {
                let given = v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string());
                return err_response(
                    &state,
                    StatusCode::BAD_REQUEST,
                    "apiModels.invalidSource",
                    &[("source", given)],
                )
                .await;
            }
        },
    };

    // ── 前置检查 ──
    let decl = match find_model_decl(&state, &module_id, &model_id).await {
        Ok(d) => d,
        Err(resp) => return resp,
    };
    let key = download_key(&module_id, &model_id);
    // std MutexGuard 不得跨 await（!Send）：先在短临界区内取标志，再构造响应
    let already_downloading = {
        let map = state.downloads.lock().unwrap_or_else(|e| e.into_inner());
        map.get(&key).is_some_and(|e| e.state == "downloading")
    };
    if already_downloading {
        return err_response(
            &state,
            StatusCode::CONFLICT,
            "apiModels.downloadInProgress",
            &[],
        )
        .await;
    }
    let mgr = build_model_manager(&state).await;
    if mgr.is_model_present(&decl.target_dir) {
        return err_response(
            &state,
            StatusCode::CONFLICT,
            "apiModels.modelAlreadyExists",
            &[],
        )
        .await;
    }
    let mut venv_python = venv_python_path(&state.root, &module_id);
    if !venv_python.exists() {
        // 自动准备 Python 环境：化解全新安装"下载需要 venv、启动又需要模型"的死锁
        let manifest = match find_module_manifest(&state, &module_id).await {
            Some(mf) => mf,
            None => {
                return err_response(
                    &state,
                    StatusCode::NOT_FOUND,
                    "apiModels.moduleNotFound",
                    &[("module_id", module_id.clone())],
                )
                .await;
            }
        };
        let (python_cfg, network_cfg) = {
            let cfg = state.config.read().await;
            (cfg.python.clone(), cfg.network.clone())
        };
        let root = state.root.clone();
        let mid = module_id.clone();
        let py_ver = manifest.runtime.python_version.clone().unwrap_or_default();
        let req_rel = manifest
            .runtime
            .requirements
            .clone()
            .unwrap_or_else(|| "requirements.txt".to_string());
        info!(module_id = %module_id, "venv missing, preparing Python environment before download");
        let prep = tokio::task::spawn_blocking(move || {
            let env_mgr =
                ep_core::env::EnvManager::new(&root, &python_cfg).with_network(&network_cfg);
            let req_path = root.join("modules").join(&mid).join(req_rel);
            env_mgr.ensure_venv(&mid, &py_ver, &req_path)
        })
        .await;
        match prep {
            Ok(Ok(path)) => venv_python = path,
            Ok(Err(e)) => {
                return err_response(
                    &state,
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "apiModels.venvPrepFailed",
                    &[("detail", format!("{e:#}"))],
                )
                .await;
            }
            Err(e) => {
                return err_response(
                    &state,
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "apiModels.venvPrepPanicked",
                    &[("detail", e.to_string())],
                )
                .await;
            }
        }
    }
    // 请求未指定 source 时回退配置 models.default_source（仅当该源在模型可用源内）
    let source = match source {
        Some(s) => Some(s),
        None => {
            let cfg = state.config.read().await;
            parse_model_source(&cfg.models.default_source)
                .filter(|s| decl.available_sources().contains(s))
        }
    };
    // 下载源可用性（主源 / mirror）先行校验，失败为用户输入错误 → 400。
    // 不透传 ep-core resolve() 的错误：daemon 按 available_sources 自行本地化。
    let effective_source = source.unwrap_or(decl.source);
    if !decl.available_sources().contains(&effective_source) {
        let available = decl
            .available_sources()
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        return err_response(
            &state,
            StatusCode::BAD_REQUEST,
            "apiModels.sourceUnavailable",
            &[
                ("source", effective_source.as_str().to_string()),
                ("available", available),
            ],
        )
        .await;
    }
    // 其余 resolve 失败（声明缺 repo_id / url 等清单缺陷）→ 兜底本地化
    if let Err(e) = decl.resolve(source) {
        return err_response(
            &state,
            StatusCode::BAD_REQUEST,
            "apiModels.sourceResolveFailed",
            &[("detail", e.to_string())],
        )
        .await;
    }

    // ── 启动下载（ep-core 内部 spawn python 子进程 + 轮询目录大小） ──
    let config = { state.config.read().await.clone() };
    let mut handle = match mgr.execute_download_with_progress(
        &module_id,
        &decl,
        &venv_python,
        &config,
        source,
    ) {
        Ok(h) => h,
        Err(e) => {
            return err_response(
                &state,
                StatusCode::INTERNAL_SERVER_ERROR,
                "apiModels.downloadStartFailed",
                &[("detail", e.to_string())],
            )
            .await;
        }
    };

    // 写入下载记录（短临界区；再查一次防并发重复提交，冲突则取消刚启动的下载）。
    // std MutexGuard 不得跨 await（!Send）：临界区内只做判定与插入，响应在锁外构造。
    let concurrent_submit = {
        let mut map = state.downloads.lock().unwrap_or_else(|e| e.into_inner());
        if map.get(&key).is_some_and(|e| e.state == "downloading") {
            true
        } else {
            map.insert(
                key.clone(),
                DownloadEntry {
                    module_id: module_id.clone(),
                    model_id: model_id.clone(),
                    source: source.unwrap_or(decl.source).as_str().to_string(),
                    percent: 0.0,
                    bytes: 0,
                    state: "downloading".to_string(),
                    started_at: chrono::Utc::now().to_rfc3339(),
                },
            );
            false
        }
    };
    if concurrent_submit {
        handle.cancel();
        return err_response(
            &state,
            StatusCode::CONFLICT,
            "apiModels.downloadInProgress",
            &[],
        )
        .await;
    }

    // 立即广播一条初始进度，方便刚错过 202 响应的客户端同步状态
    let _ = state.model_download_tx.send(WsMessage::ModelDownload {
        module_id: module_id.clone(),
        model_id: model_id.clone(),
        percent: 0.0,
        state: "downloading".to_string(),
        bytes: 0,
    });

    // 后台监控任务：中继进度事件，结束时落终态。
    // 扩展注释：本期不提供取消 API——DownloadHandle::cancel() 已就绪，
    // 后续如需取消端点，把该任务持有的句柄存入 state 即可。
    let downloads = Arc::clone(&state.downloads);
    let ws_tx = state.model_download_tx.clone();
    let task_key = key;
    let task_module = module_id.clone();
    let task_model = model_id.clone();
    tokio::spawn(async move {
        monitor_download(handle, downloads, ws_tx, task_key, task_module, task_model).await;
    });

    info!(module_id = %module_id, model_id = %model_id, "API: model download accepted");
    (StatusCode::ACCEPTED, Json(json!({ "ok": true })))
}

/// 中继下载进度直到结束：每条进度更新 downloads 表并广播 WS；
/// 结束后把 entry 置为 completed / failed / cancelled 并发送最后一条 WS 消息。
///
/// std Mutex 仅覆盖表更新，短临界区、不跨 await；广播发送失败一律忽略。
async fn monitor_download(
    handle: ep_core::model::DownloadHandle,
    downloads: Arc<std::sync::Mutex<std::collections::HashMap<String, DownloadEntry>>>,
    ws_tx: tokio::sync::broadcast::Sender<WsMessage>,
    key: String,
    module_id: String,
    model_id: String,
) {
    let mut rx = handle.subscribe_progress();
    let wait_fut = handle.wait();
    tokio::pin!(wait_fut);

    let mut last: Option<DownloadProgress> = None;
    let mut wait_result: Option<Result<u64, String>> = None;

    loop {
        tokio::select! {
            recv = rx.recv() => match recv {
                Ok(p) => {
                    relay_download_progress(&downloads, &ws_tx, &key, &p);
                    last = Some(p);
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    debug!(module_id = %module_id, model_id = %model_id, lagged = n, "download progress events lagged");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            },
            res = &mut wait_fut => {
                wait_result = Some(res);
                // 抽干队列中剩余事件（含 wait 返回前最后入队的终态事件）
                while let Ok(p) = rx.try_recv() {
                    relay_download_progress(&downloads, &ws_tx, &key, &p);
                    last = Some(p);
                }
                break;
            }
        }
    }

    // 终态判定：优先监督任务的终态进度事件，异常路径回退 wait 返回值
    let (percent_keep, bytes_keep) = last
        .as_ref()
        .map(|p| (p.percent, p.bytes))
        .unwrap_or((0.0, 0));
    let (final_state, final_percent, final_bytes) = match last.as_ref().map(|p| &p.state) {
        Some(DownloadState::Completed) => ("completed", 100.0, bytes_keep),
        Some(DownloadState::Failed(_)) => ("failed", percent_keep, bytes_keep),
        Some(DownloadState::Cancelled) => ("cancelled", percent_keep, bytes_keep),
        _ => match wait_result {
            Some(Ok(bytes)) => ("completed", 100.0, bytes),
            Some(Err(msg)) if msg.contains("取消") => ("cancelled", percent_keep, bytes_keep),
            Some(Err(err)) => {
                warn!(module_id = %module_id, model_id = %model_id, error = %err, "model download failed");
                ("failed", percent_keep, bytes_keep)
            }
            None => ("failed", percent_keep, bytes_keep),
        },
    };

    {
        let mut map = downloads.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = map.get_mut(&key) {
            entry.percent = final_percent;
            entry.bytes = final_bytes;
            entry.state = final_state.to_string();
        }
    }
    let _ = ws_tx.send(WsMessage::ModelDownload {
        module_id: module_id.clone(),
        model_id: model_id.clone(),
        percent: final_percent,
        state: final_state.to_string(),
        bytes: final_bytes,
    });
    info!(
        module_id = %module_id,
        model_id = %model_id,
        state = final_state,
        bytes = final_bytes,
        "model download finished"
    );
}

/// GET /api/models/downloads — 全部下载记录（按 started_at 升序）。
///
/// 响应体为数组，元素即 `DownloadEntry` 的蛇形命名字段（前端契约 ModelDownloadStatus[]）。
async fn list_downloads(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    let mut entries: Vec<DownloadEntry> = {
        let map = state.downloads.lock().unwrap_or_else(|e| e.into_inner());
        map.values().cloned().collect()
    };
    entries.sort_by(|a, b| a.started_at.cmp(&b.started_at));
    (StatusCode::OK, Json(json!(entries)))
}

/// DELETE /api/models/:module_id/:model_id — 删除本地模型目录（含 .ep_meta.json）
async fn delete_model(
    State(state): State<Arc<AppState>>,
    Path((module_id, model_id)): Path<(String, String)>,
) -> (StatusCode, Json<Value>) {
    let decl = match find_model_decl(&state, &module_id, &model_id).await {
        Ok(d) => d,
        Err(resp) => return resp,
    };
    let key = download_key(&module_id, &model_id);
    // std MutexGuard 不得跨 await（!Send）：先在短临界区内取标志，再构造响应
    let downloading = {
        let map = state.downloads.lock().unwrap_or_else(|e| e.into_inner());
        map.get(&key).is_some_and(|e| e.state == "downloading")
    };
    if downloading {
        return err_response(&state, StatusCode::CONFLICT, "apiModels.deleteInProgress", &[])
            .await;
    }

    let mgr = build_model_manager(&state).await;
    let dir = mgr.model_dir(&decl.target_dir);
    if !dir.is_dir() {
        return err_response(&state, StatusCode::NOT_FOUND, "apiModels.modelDirAbsent", &[])
            .await;
    }
    if let Err(e) = tokio::fs::remove_dir_all(&dir).await {
        return err_response(
            &state,
            StatusCode::INTERNAL_SERVER_ERROR,
            "apiModels.deleteFailed",
            &[("detail", e.to_string())],
        )
        .await;
    }

    // 保持列表干净：移除该模型的历史下载记录（downloading 已在上方拦截）
    {
        let mut map = state.downloads.lock().unwrap_or_else(|e| e.into_inner());
        map.remove(&key);
    }

    info!(module_id = %module_id, model_id = %model_id, dir = %dir.display(), "API: model deleted");
    (StatusCode::OK, Json(json!({ "ok": true })))
}

/// POST /api/models/:module_id/:model_id/check-update — 检查模型更新
///
/// 基于 ep-core `check_update_available` 的结果（best-effort，永不 Err），
/// 由 daemon 按已知事实本地化 reason（不透传 ep-core 的中文文案）：
/// - URL 来源 → `updateUnsupported`；无下载元数据 → `updateNoMeta`（均本地短路，不触网）
/// - `available=true` → `updateAvailable`（{{info}} 为远端最后修改时间）
/// - `available=false` → `updateUpToDate`
///
/// 响应形状不变：`{"available": bool, "reason": 本地化说明}`。
async fn check_model_update(
    State(state): State<Arc<AppState>>,
    Path((module_id, model_id)): Path<(String, String)>,
) -> (StatusCode, Json<Value>) {
    let decl = match find_model_decl(&state, &module_id, &model_id).await {
        Ok(d) => d,
        Err(resp) => return resp,
    };
    let lang = state.lang().await;

    // URL 来源不支持更新检查：daemon 已知事实，直接短路
    if decl.source == ModelSource::Url {
        return (
            StatusCode::OK,
            Json(json!({
                "available": false,
                "reason": ep_core::i18n::t(&lang, "apiModels.updateUnsupported", &[]),
            })),
        );
    }

    let mgr = build_model_manager(&state).await;

    // 无下载元数据 → 无法比较：daemon 已知事实，直接短路
    if mgr.read_meta(&decl.target_dir).is_none() {
        return (
            StatusCode::OK,
            Json(json!({
                "available": false,
                "reason": ep_core::i18n::t(&lang, "apiModels.updateNoMeta", &[]),
            })),
        );
    }

    let result = mgr.check_update_available(&decl).await;
    let reason = if result.available {
        let info = result.remote_modified.clone().unwrap_or_default();
        ep_core::i18n::t(&lang, "apiModels.updateAvailable", &[("info", &info)])
    } else {
        ep_core::i18n::t(&lang, "apiModels.updateUpToDate", &[])
    };
    (
        StatusCode::OK,
        Json(json!({
            "available": result.available,
            "reason": reason,
        })),
    )
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use ep_core::config::AppConfig;
    use ep_core::module::discovery::{DiscoveredModule, DiscoveryStatus};
    use ep_core::module::manifest::{
        ComputeConfig, InterfaceConfig, InterfaceType, ModuleInfo, RuntimeConfig, RuntimeType,
    };
    use ep_core::port::PortManager;
    use ep_core::types::{ComputeBackend, ModuleCategory};

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    const MODULE_ID: &str = "test-mod";
    const MODEL_ID: &str = "m1";
    const URL_MODEL_ID: &str = "m2";
    const TARGET_DIR: &str = "test-model-dir";

    fn hf_model_decl() -> ModelDecl {
        ModelDecl {
            id: MODEL_ID.into(),
            name: "测试模型".into(),
            source: ModelSource::Huggingface,
            repo_id: Some("org/test-model".into()),
            url: None,
            target_dir: TARGET_DIR.into(),
            revision: None,
            size_estimate_mb: Some(100),
            default: true,
            mirrors: vec![],
        }
    }

    fn url_model_decl() -> ModelDecl {
        ModelDecl {
            id: URL_MODEL_ID.into(),
            name: "URL 模型".into(),
            source: ModelSource::Url,
            repo_id: None,
            url: Some("https://example.com/model.bin".into()),
            target_dir: "url-model-dir".into(),
            revision: None,
            size_estimate_mb: None,
            default: false,
            mirrors: vec![],
        }
    }

    fn test_manifest() -> ModuleManifest {
        ModuleManifest {
            module: ModuleInfo {
                id: MODULE_ID.into(),
                name: "测试模块".into(),
                version: "1.0.0".into(),
                description: "测试用模块".into(),
                category: ModuleCategory::Asr,
                genre: "test".into(),
                authors: vec![],
                license: None,
                homepage: None,
                tags: vec![],
            },
            runtime: RuntimeConfig {
                runtime_type: RuntimeType::Python,
                python_version: Some(">=3.10".into()),
                requirements: None,
                entrypoint: None,
                start_command: None,
                binaries: None,
            },
            compute: ComputeConfig {
                backends: vec![ComputeBackend::Cpu],
                default_backend: None,
                vram_estimate_mb: None,
                min_vram_mb: None,
                env: None,
            },
            models: vec![hf_model_decl(), url_model_decl()],
            interface: InterfaceConfig {
                interface_type: InterfaceType::Http,
                health_endpoint: None,
                ready_timeout_secs: None,
                working_dir: None,
                capabilities: vec![],
            },
        }
    }

    /// 唯一 tempdir root + 单模块 manifest 的测试 AppState（默认 zh-CN）
    fn test_state() -> Arc<AppState> {
        test_state_with_language("zh-CN")
    }

    /// 同 test_state，可指定 UI 语言（config.general.language）
    fn test_state_with_language(language: &str) -> Arc<AppState> {
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "ep-daemon-models-test-{}-{seq}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let mut config = AppConfig::default();
        config.general.language = language.to_string();
        let modules = vec![DiscoveredModule {
            manifest: Some(test_manifest()),
            path: root.join("modules").join(MODULE_ID),
            status: DiscoveryStatus::Valid,
        }];
        Arc::new(AppState::new(
            root,
            config,
            vec![],
            modules,
            PortManager::new(18000, 19000),
        ))
    }

    fn seed_entry(state: &AppState, download_state: &str, started_at: &str) {
        state.downloads.lock().unwrap().insert(
            download_key(MODULE_ID, MODEL_ID),
            DownloadEntry {
                module_id: MODULE_ID.into(),
                model_id: MODEL_ID.into(),
                source: "huggingface".into(),
                percent: 10.0,
                bytes: 123,
                state: download_state.into(),
                started_at: started_at.into(),
            },
        );
    }

    // ── download：参数与前置检查 ─────────────────────────────────────────

    #[tokio::test]
    async fn test_download_unknown_module_404() {
        let state = test_state();
        let (status, json) = download_model(
            State(state),
            Path("ghost-mod".to_string()),
            Some(Json(json!({ "model_id": MODEL_ID }))),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(json.0["error"].as_str().unwrap().contains("不存在"));
    }

    #[tokio::test]
    async fn test_download_unknown_model_404() {
        let state = test_state();
        let (status, json) = download_model(
            State(state),
            Path(MODULE_ID.to_string()),
            Some(Json(json!({ "model_id": "ghost-model" }))),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(json.0["error"].as_str().unwrap().contains("不存在"));
    }

    #[tokio::test]
    async fn test_download_missing_model_id_400() {
        let state = test_state();
        let (status, json) = download_model(
            State(state),
            Path(MODULE_ID.to_string()),
            Some(Json(json!({}))),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(json.0["error"].as_str().unwrap().contains("model_id"));
    }

    #[tokio::test]
    async fn test_download_invalid_source_400() {
        let state = test_state();
        let (status, json) = download_model(
            State(state),
            Path(MODULE_ID.to_string()),
            Some(Json(json!({ "model_id": MODEL_ID, "source": "ftp" }))),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let err = json.0["error"].as_str().unwrap();
        assert!(err.contains("下载源"), "err: {err}");
    }

    #[tokio::test]
    async fn test_download_duplicate_409() {
        let state = test_state();
        seed_entry(&state, "downloading", "2026-08-04T00:00:00+00:00");
        let (status, json) = download_model(
            State(state),
            Path(MODULE_ID.to_string()),
            Some(Json(json!({ "model_id": MODEL_ID }))),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(json.0["error"], "该模型正在下载中");
    }

    #[tokio::test]
    async fn test_download_already_ready_409() {
        let state = test_state();
        let dir = state.root.join("models").join(TARGET_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("model.bin"), b"weights").unwrap();

        let (status, json) = download_model(
            State(state),
            Path(MODULE_ID.to_string()),
            Some(Json(json!({ "model_id": MODEL_ID }))),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(json.0["error"].as_str().unwrap().contains("模型已存在"));
    }

    #[tokio::test]
    async fn test_download_auto_venv_prep_failure_500() {
        // venv 缺失时下载会自动准备 Python 环境；构造一个不可能的 Python 版本
        // 使 ensure_venv 必然失败 → 500 中文错误（而非旧的 412 死锁提示）
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "ep-daemon-models-autovenv-{seq}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let mut manifest = test_manifest();
        manifest.runtime.python_version = Some("3.999".into());
        let modules = vec![DiscoveredModule {
            manifest: Some(manifest),
            path: root.join("modules").join(MODULE_ID),
            status: DiscoveryStatus::Valid,
        }];
        let state = Arc::new(AppState::new(
            root,
            AppConfig::default(),
            vec![],
            modules,
            PortManager::new(18000, 19000),
        ));
        let (status, json) = download_model(
            State(state),
            Path(MODULE_ID.to_string()),
            Some(Json(json!({ "model_id": MODEL_ID }))),
        )
        .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(json.0["error"]
            .as_str()
            .unwrap()
            .contains("Python 环境准备失败"));
    }

    // ── download：受理 + 监督任务落终态（用假 python 脚本，不依赖真实环境） ──

    #[tokio::test]
    #[cfg(unix)]
    async fn test_download_accepted_and_completes() {
        use std::os::unix::fs::PermissionsExt;

        let state = test_state();
        // 假 venv python：立即成功退出的 shell 脚本
        let venv_dir = state
            .root
            .join("runtime")
            .join("venvs")
            .join(MODULE_ID)
            .join("bin");
        std::fs::create_dir_all(&venv_dir).unwrap();
        let py = venv_dir.join("python");
        std::fs::write(&py, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&py, std::fs::Permissions::from_mode(0o755)).unwrap();

        let mut rx = state.model_download_tx.subscribe();
        let (status, json) = download_model(
            State(state.clone()),
            Path(MODULE_ID.to_string()),
            Some(Json(json!({ "model_id": MODEL_ID }))),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(json.0["ok"], true);

        // 等待终态 WS 消息（子进程立即成功退出 → completed）
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut saw_completed = false;
        while tokio::time::Instant::now() < deadline && !saw_completed {
            match tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv()).await {
                Ok(Ok(WsMessage::ModelDownload { state: s, .. })) if s == "completed" => {
                    saw_completed = true;
                }
                _ => {}
            }
        }
        assert!(saw_completed, "expected a 'completed' WS message");

        let map = state.downloads.lock().unwrap();
        let entry = map.get(&download_key(MODULE_ID, MODEL_ID)).unwrap();
        assert_eq!(entry.state, "completed");
        assert_eq!(entry.percent, 100.0);
    }

    // ── downloads 列表 ───────────────────────────────────────────────────

    #[tokio::test]
    async fn test_list_downloads_shape_and_sort() {
        let state = test_state();
        {
            let mut map = state.downloads.lock().unwrap();
            map.insert(
                "b-mod:b-model".into(),
                DownloadEntry {
                    module_id: "b-mod".into(),
                    model_id: "b-model".into(),
                    source: "modelscope".into(),
                    percent: 55.0,
                    bytes: 999,
                    state: "downloading".into(),
                    started_at: "2026-08-04T10:01:00+00:00".into(),
                },
            );
            map.insert(
                "a-mod:a-model".into(),
                DownloadEntry {
                    module_id: "a-mod".into(),
                    model_id: "a-model".into(),
                    source: "huggingface".into(),
                    percent: 100.0,
                    bytes: 42,
                    state: "completed".into(),
                    started_at: "2026-08-04T10:00:00+00:00".into(),
                },
            );
        }

        let (status, json) = list_downloads(State(state)).await;
        assert_eq!(status, StatusCode::OK);
        let arr = json.0.as_array().expect("downloads must be a JSON array");
        assert_eq!(arr.len(), 2);
        // 按 started_at 升序
        assert_eq!(arr[0]["model_id"], "a-model");
        assert_eq!(arr[1]["model_id"], "b-model");
        // 蛇形命名字段形状（前端契约 ModelDownloadStatus[]）
        for key in [
            "module_id",
            "model_id",
            "source",
            "percent",
            "bytes",
            "state",
            "started_at",
        ] {
            assert!(arr[0].get(key).is_some(), "missing key '{key}'");
        }
        assert_eq!(arr[0]["state"], "completed");
        assert_eq!(arr[1]["state"], "downloading");
    }

    // ── delete ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_delete_nonexistent_404() {
        let state = test_state();
        let (status, json) = delete_model(
            State(state),
            Path((MODULE_ID.to_string(), MODEL_ID.to_string())),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(json.0["error"].as_str().unwrap().contains("不存在"));
    }

    #[tokio::test]
    async fn test_delete_while_downloading_409() {
        let state = test_state();
        seed_entry(&state, "downloading", "2026-08-04T00:00:00+00:00");
        let (status, json) = delete_model(
            State(state),
            Path((MODULE_ID.to_string(), MODEL_ID.to_string())),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(json.0["error"].as_str().unwrap().contains("正在下载"));
    }

    #[tokio::test]
    async fn test_delete_success_removes_dir_and_entry() {
        let state = test_state();
        let dir = state.root.join("models").join(TARGET_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("model.bin"), b"weights").unwrap();
        std::fs::write(dir.join(".ep_meta.json"), "{}").unwrap();
        seed_entry(&state, "completed", "2026-08-04T00:00:00+00:00");

        let (status, json) = delete_model(
            State(state.clone()),
            Path((MODULE_ID.to_string(), MODEL_ID.to_string())),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json.0["ok"], true);
        assert!(!dir.exists(), "model dir must be removed");
        assert!(
            state
                .downloads
                .lock()
                .unwrap()
                .get(&download_key(MODULE_ID, MODEL_ID))
                .is_none(),
            "download entry must be cleaned up"
        );
    }

    // ── check-update ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_check_update_unknown_model_404() {
        let state = test_state();
        let (status, _) = check_model_update(
            State(state),
            Path((MODULE_ID.to_string(), "ghost".to_string())),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_check_update_url_source_localized() {
        let state = test_state();
        let (status, json) = check_model_update(
            State(state),
            Path((MODULE_ID.to_string(), URL_MODEL_ID.to_string())),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json.0["available"], false);
        assert_eq!(json.0["reason"], "URL 来源不支持更新检查");
    }

    #[tokio::test]
    async fn test_check_update_missing_meta_localized() {
        // 无 .ep_meta.json → daemon 本地短路返回"缺少下载元数据"（不触网）
        let state = test_state();
        let (status, json) = check_model_update(
            State(state),
            Path((MODULE_ID.to_string(), MODEL_ID.to_string())),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json.0["available"], false);
        assert_eq!(json.0["reason"], "缺少下载元数据，无法比较");
    }

    // language=en → 下载错误返回英文文案（404 模块不存在 / 400 非法下载源）
    #[tokio::test]
    async fn test_download_errors_in_english_when_language_en() {
        let state = test_state_with_language("en");

        let (status, json) = download_model(
            State(state.clone()),
            Path("ghost-mod".to_string()),
            Some(Json(json!({ "model_id": MODEL_ID }))),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(json.0["error"], "Module 'ghost-mod' does not exist");

        let (status, json) = download_model(
            State(state),
            Path(MODULE_ID.to_string()),
            Some(Json(json!({ "model_id": MODEL_ID, "source": "ftp" }))),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let err = json.0["error"].as_str().unwrap();
        assert!(
            err.contains("Invalid download source"),
            "expected English error, got: {err}"
        );
    }

    #[test]
    fn test_parse_model_source() {
        assert_eq!(
            parse_model_source("huggingface"),
            Some(ModelSource::Huggingface)
        );
        assert_eq!(
            parse_model_source(" modelscope "),
            Some(ModelSource::Modelscope)
        );
        assert_eq!(parse_model_source("url"), Some(ModelSource::Url));
        assert_eq!(parse_model_source(""), None);
        assert_eq!(parse_model_source("bad-source"), None);
    }
}