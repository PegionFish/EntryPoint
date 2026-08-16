//! 模型管理 API — 状态查询、本地导入、下载、删除、更新检查、tags、下载取消
//!
//! 下载走 ep-core `execute_download_with_progress`（python 子进程 + 目录大小轮询），
//! 进度写入 `state.downloads` 并经 `state.model_download_tx` 广播到 /ws。
//!
//! Wave 2（B6）：
//! - `PUT /models/{m}/{mid}/tags`（§8.1）：全量覆写 `.ep_meta.json` 的 tags；
//! - `POST /models/{m}/{mid}/cancel-download`（P2-6）：经 ep-core DownloadHandle
//!   取消进行中下载（排队中 → 直接标记取消）；
//! - 下载并发闸（P2-1）：`config.models.max_concurrent_downloads` 经本文件内
//!   static Semaphore 生效，超额请求以 `queued` 状态排队（见 [`download_gate`]）；
//! - pack 来源展示（§5.1）：模型列表透传 meta 的 pack_id/qualified_id/tags。

use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{delete, get, post, put},
    Json,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, oneshot};
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
        .route("/models/{module_id}/{model_id}/tags", put(set_model_tags))
        .route(
            "/models/{module_id}/{model_id}/cancel-download",
            post(cancel_model_download),
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

#[derive(Debug, Deserialize)]
pub struct TagsRequest {
    /// 全量标签列表（§8.1：`{tags: []}`；空数组 = 清空全部标签）
    pub tags: Vec<String>,
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

/// 模块 venv python 解释器路径：统一转发共享助手（任务 #10 去重，
/// 双平台口径见 [`super::module_venv_python_path`]）；仅测试 fixture 使用
#[cfg(test)]
fn venv_python_path(root: &std::path::Path, module_id: &str) -> PathBuf {
    super::module_venv_python_path(root, module_id)
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

// ─── 下载并发闸（P2-1）与取消信号注册表（P2-6） ─────────────────────────────

/// 下载并发闸（P2-1）：按 `config.models.max_concurrent_downloads` 懒构建。
///
/// 闸门挂在 models.rs 内 static（不改 state.rs）；`AppState.downloads` 仍是
/// 下载状态的唯一事实源，闸门只决定"立即开始 / 排队"。
///
/// **排队行为（文档化，C1 消费）**：
/// - `POST /models/{m}/download` 受理时先 `try_acquire` 闸门：
///   - 有空位 → 立即启动下载，entry 状态 `downloading`，响应 `queued: false`；
///   - 无空位 → 仍返回 202，entry 落 `queued` 状态，响应 `queued: true`；
///     空位释放后按提交顺序（Semaphore 公平性）自动启动并转 `downloading`；
/// - 排队可见性：`GET /models/downloads` 与 WS `model_download` 消息均带
///   `queued` / `downloading` 状态流转；
/// - 排队中的下载可经 `cancel-download` 直接取消（不占闸门）；
/// - `max_concurrent_downloads` 运行时变更 → 按新值懒重建：在途下载仍占原
///   闸门至结束，新下载按新上限（过渡期总并发 = 旧在途数 + 新上限）；
/// - 配置 0 → 按 1 处理（避免永久排队死锁）。
static DOWNLOAD_GATE: std::sync::Mutex<Option<(u32, Arc<Semaphore>)>> =
    std::sync::Mutex::new(None);

/// 获取（懒构建/重建）下载并发闸信号量
fn download_gate(max_concurrent: u32) -> Arc<Semaphore> {
    let max = max_concurrent.max(1);
    let mut guard = DOWNLOAD_GATE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((current, sem)) = guard.as_ref() {
        if *current == max {
            return sem.clone();
        }
        info!(
            from = *current,
            to = max,
            "max_concurrent_downloads changed, rebuilding download gate"
        );
    }
    let sem = Arc::new(Semaphore::new(max as usize));
    *guard = Some((max, sem.clone()));
    sem
}

/// 进行中下载的取消信号注册表（P2-6）：key（见 [`download_key`]）→ oneshot 发送端。
///
/// 监督任务 spawn 前登记、终态落定后移除；取消端点 `send(())` 触发监督任务
/// 调用 ep-core `DownloadHandle::cancel()`（kill python 子进程）。
///
/// `Option` 包装：`HashMap::new` 非 const fn，static 惰性初始化（首次登记时创建）。
static DOWNLOAD_CANCEL_TXS: std::sync::Mutex<
    Option<std::collections::HashMap<String, oneshot::Sender<()>>>,
> = std::sync::Mutex::new(None);

/// 下载 entry 是否"活跃"（排队中或下载中）——重复提交 / 删除保护 / 取消定位共用
fn is_active_download(state: &str) -> bool {
    matches!(state, "queued" | "downloading")
}

/// 终态下载条目 TTL（秒）：completed/failed/cancelled 条目超过该时长后在
/// 读取路径淘汰（P2 修复），防 downloads 表内存长期增长。活跃条目不淘汰。
const DOWNLOADS_TTL_SECS: i64 = 3600;

/// 下载状态是否终态（completed / failed / cancelled）
fn is_terminal_download_state(state: &str) -> bool {
    matches!(state, "completed" | "failed" | "cancelled")
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
                // pack 来源展示（§5.1）：透传 .ep_meta.json 的 pack_id/qualified_id/tags。
                // qualified_id 优先 meta（整合包导入时由 B1 写入），无 meta 或 meta
                // 未记录时回退 manifest 声明值；无 meta 时 pack_id=null / tags=[]。
                let meta = mgr.read_meta(&model.target_dir);
                let qualified_id = meta
                    .as_ref()
                    .and_then(|m| m.qualified_id.clone())
                    .or_else(|| model.qualified_id.clone());
                let pack_id = meta.as_ref().and_then(|m| m.pack_id.clone());
                let tags = meta
                    .as_ref()
                    .map(|m| m.tags.clone())
                    .unwrap_or_default();
                json!({
                    "model_id": model.id,
                    "name": model.name,
                    "target_dir": model.target_dir,
                    "status": status.to_string(),
                    "source": model.source.as_str(),
                    "size_estimate_mb": model.size_estimate_mb,
                    "available_sources": model.available_sources(),
                    "qualified_id": qualified_id,
                    "pack_id": pack_id,
                    "tags": tags,
                    "vram_estimate_mb": model.vram_estimate_mb,
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

    // get_model_info 按 manifest.models 顺序映射，zip 安全；
    // pack 来源展示（§5.1）：透传 meta 的 pack_id/qualified_id/tags，
    // qualified_id 无 meta 记录时回退 manifest 声明值（同 list_models）。
    let models: Vec<Value> = manifest
        .models
        .iter()
        .zip(infos.iter())
        .map(|(decl, info)| {
            let meta = mgr.read_meta(&info.target_dir);
            let qualified_id = meta
                .as_ref()
                .and_then(|m| m.qualified_id.clone())
                .or_else(|| decl.qualified_id.clone());
            let pack_id = meta.as_ref().and_then(|m| m.pack_id.clone());
            let tags = meta
                .as_ref()
                .map(|m| m.tags.clone())
                .unwrap_or_default();
            json!({
                "model_id": info.model_id,
                "name": info.name,
                "target_dir": info.target_dir,
                "status": info.status.to_string(),
                "size_bytes": info.size_bytes,
                "file_count": info.file_count,
                "local_cache_path": info.local_cache_path,
                "available_sources": info.available_sources,
                "qualified_id": qualified_id,
                "pack_id": pack_id,
                "tags": tags,
                "vram_estimate_mb": decl.vram_estimate_mb,
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

    // P2 修复：与 upload 的 target_blocked 语义一致——目标目录已存在且非空
    // （或为文件）→ 409，拒绝合并写入（旧实现直接 create_dir_all + 合并复制）
    if super::upload::target_blocked(&target_dir).await {
        return err_response(
            &state,
            StatusCode::CONFLICT,
            "apiModels.uploadConflict",
            &[("model_id", req.model_id.clone())],
        )
        .await;
    }

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
                qualified_id: None,
                tags: vec![],
                pack_id: None,
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
/// - 202 已受理（后台任务执行下载，进度经 state.downloads + /ws 广播）。
///   响应体 `{"ok": true, "queued": bool}`（B6 起新增 `queued` 字段）：
///   `queued: false` = 闸门有空位立即开始；`queued: true` = 并发闸
///   （P2-1，`models.max_concurrent_downloads`）已满，entry 落 `queued`
///   状态排队，空位释放后自动启动（排队行为详见 [`download_gate`]）。
/// - 400 请求体非法 / 下载源不可用；404 模块或模型不存在
/// - 409 正在下载中（含排队中）或模型已存在
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
    // std MutexGuard 不得跨 await（!Send）：先在短临界区内取标志，再构造响应。
    // 排队中（queued）与下载中（downloading）同属活跃下载，重复提交一律 409。
    let already_active = {
        let map = state.downloads.lock().unwrap_or_else(|e| e.into_inner());
        map.get(&key).is_some_and(|e| is_active_download(&e.state))
    };
    if already_active {
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
    // venv 就绪门禁（任务 #10，与手动启动/自动拉起同源的共享助手）：
    // is_venv_ready 哈希门禁修复"半壳 venv"误判；venv 缺失自动准备，
    // 化解全新安装"下载需要 venv、启动又需要模型"的死锁。
    // 模块未被发现时保留旧语义：venv 存在 → 继续用既有解释器；否则 → 404。
    let venv_python = match find_module_manifest(&state, &module_id).await {
        Some(mf) => match super::ensure_module_venv_ready(&state, &module_id, &mf).await {
            Ok(path) => path,
            Err(detail) => {
                return err_response(
                    &state,
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "apiModels.venvPrepFailed",
                    &[("detail", detail)],
                )
                .await;
            }
        },
        None => {
            let legacy = super::module_venv_python_path(&state.root, &module_id);
            if !legacy.exists() {
                return err_response(
                    &state,
                    StatusCode::NOT_FOUND,
                    "apiModels.moduleNotFound",
                    &[("module_id", module_id.clone())],
                )
                .await;
            }
            legacy
        }
    };
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

    // ── 并发闸（P2-1）：有空位立即启动，满员落 queued 排队（见 download_gate） ──
    let max_concurrent = { state.config.read().await.models.max_concurrent_downloads };
    let gate = download_gate(max_concurrent);
    let entry_source = source.unwrap_or(decl.source).as_str().to_string();

    let permit = match gate.clone().try_acquire_owned() {
        Ok(p) => p,
        Err(_) => {
            // ── 排队路径：闸门满员，entry 落 queued，202 立即返回 ──
            // 短临界区插入记录；再查一次防并发重复提交（queued 与 downloading 均冲突）
            let concurrent_submit = {
                let mut map = state.downloads.lock().unwrap_or_else(|e| e.into_inner());
                if map.get(&key).is_some_and(|e| is_active_download(&e.state)) {
                    true
                } else {
                    map.insert(
                        key.clone(),
                        DownloadEntry {
                            module_id: module_id.clone(),
                            model_id: model_id.clone(),
                            source: entry_source,
                            percent: 0.0,
                            bytes: 0,
                            state: "queued".to_string(),
                            started_at: chrono::Utc::now().to_rfc3339(),
                        },
                    );
                    false
                }
            };
            if concurrent_submit {
                return err_response(
                    &state,
                    StatusCode::CONFLICT,
                    "apiModels.downloadInProgress",
                    &[],
                )
                .await;
            }
            let _ = state.model_download_tx.send(WsMessage::ModelDownload {
                module_id: module_id.clone(),
                model_id: model_id.clone(),
                percent: 0.0,
                state: "queued".to_string(),
                bytes: 0,
            });
            tokio::spawn(queued_download_runner(
                state, key, module_id.clone(), model_id.clone(), decl, venv_python, source, gate,
            ));
            info!(module_id = %module_id, model_id = %model_id, "API: model download queued");
            return (StatusCode::ACCEPTED, Json(json!({ "ok": true, "queued": true })));
        }
    };

    // ── 立即路径：闸门有空位，启动下载（ep-core 内部 spawn python 子进程 + 轮询目录大小） ──
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
            drop(permit);
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
        if map.get(&key).is_some_and(|e| is_active_download(&e.state)) {
            true
        } else {
            map.insert(
                key.clone(),
                DownloadEntry {
                    module_id: module_id.clone(),
                    model_id: model_id.clone(),
                    source: entry_source,
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
        drop(permit);
        return err_response(
            &state,
            StatusCode::CONFLICT,
            "apiModels.downloadInProgress",
            &[],
        )
        .await;
    }

    spawn_download_monitor(&state, key, module_id.clone(), model_id.clone(), handle, permit);

    info!(module_id = %module_id, model_id = %model_id, "API: model download accepted");
    (StatusCode::ACCEPTED, Json(json!({ "ok": true, "queued": false })))
}

/// 排队下载任务（P2-1 排队语义）：等待闸门空位后启动下载。
///
/// 等待期间 entry 被取消（cancel-download）或删除 → 静默退出归还空位；
/// 取得空位后启动下载并把 entry 由 `queued` 转 `downloading`（转换前再校验，
/// 防止与取消端点的竞态把已取消的下载拉活）。
#[allow(clippy::too_many_arguments)]
async fn queued_download_runner(
    state: Arc<AppState>,
    key: String,
    module_id: String,
    model_id: String,
    decl: ModelDecl,
    venv_python: PathBuf,
    source: Option<ModelSource>,
    gate: Arc<Semaphore>,
) {
    let permit = match gate.acquire_owned().await {
        Ok(p) => p,
        // Semaphore 不会被关闭，防御分支
        Err(_) => return,
    };

    // 等待期间可能已被取消/删除：复查 entry 仍为 queued 才继续
    let still_queued = {
        let map = state.downloads.lock().unwrap_or_else(|e| e.into_inner());
        map.get(&key).is_some_and(|e| e.state == "queued")
    };
    if !still_queued {
        debug!(module_id = %module_id, model_id = %model_id, "queued download cancelled before gate slot freed");
        return;
    }

    let mgr = build_model_manager(&state).await;

    // 启动前复查模型是否已存在：排队窗口可达分钟级，期间模型可能已被本地
    // 导入/上传落位；此时继续下载会覆盖来源 meta（如 local_import → huggingface），
    // 故终止排队任务，entry 落 failed（原因见日志），空位随 permit drop 归还。
    if mgr.is_model_present(&decl.target_dir) {
        {
            let mut map = state.downloads.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(entry) = map.get_mut(&key) {
                entry.state = "failed".to_string();
            }
        }
        let _ = state.model_download_tx.send(WsMessage::ModelDownload {
            module_id: module_id.clone(),
            model_id: model_id.clone(),
            percent: 0.0,
            state: "failed".to_string(),
            bytes: 0,
        });
        warn!(module_id = %module_id, model_id = %model_id, "queued download aborted: model already present (imported while queued)");
        return;
    }

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
            // 启动失败：entry 落终态 failed 并广播，闸门空位随 permit drop 归还
            {
                let mut map = state.downloads.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(entry) = map.get_mut(&key) {
                    entry.state = "failed".to_string();
                }
            }
            let _ = state.model_download_tx.send(WsMessage::ModelDownload {
                module_id: module_id.clone(),
                model_id: model_id.clone(),
                percent: 0.0,
                state: "failed".to_string(),
                bytes: 0,
            });
            warn!(module_id = %module_id, model_id = %model_id, error = %e, "queued model download failed to start");
            return;
        }
    };

    // queued → downloading 转换（短临界区；期间被取消则取消刚启动的下载）
    let proceed = {
        let mut map = state.downloads.lock().unwrap_or_else(|e| e.into_inner());
        match map.get_mut(&key) {
            Some(e) if e.state == "queued" => {
                e.state = "downloading".to_string();
                true
            }
            _ => false,
        }
    };
    if !proceed {
        handle.cancel();
        return;
    }

    spawn_download_monitor(&state, key, module_id, model_id, handle, permit);
}

/// 登记取消信号、广播初始 downloading 进度、spawn 监督任务（P2-6 取消接线点）。
///
/// 监督任务持有闸门 permit 直至终态（随任务结束自动归还）。
fn spawn_download_monitor(
    state: &Arc<AppState>,
    key: String,
    module_id: String,
    model_id: String,
    handle: ep_core::model::DownloadHandle,
    permit: OwnedSemaphorePermit,
) {
    let (cancel_tx, cancel_rx) = oneshot::channel::<()>();
    {
        let mut guard = DOWNLOAD_CANCEL_TXS.lock().unwrap_or_else(|e| e.into_inner());
        guard
            .get_or_insert_with(std::collections::HashMap::new)
            .insert(key.clone(), cancel_tx);
    }
    // 立即广播一条初始进度，方便刚错过 202 响应的客户端同步状态
    // （排队路径上同时承担 queued → downloading 的状态流转广播）
    let _ = state.model_download_tx.send(WsMessage::ModelDownload {
        module_id: module_id.clone(),
        model_id: model_id.clone(),
        percent: 0.0,
        state: "downloading".to_string(),
        bytes: 0,
    });
    let downloads = Arc::clone(&state.downloads);
    let ws_tx = state.model_download_tx.clone();
    tokio::spawn(async move {
        monitor_download(
            handle, downloads, ws_tx, key, module_id, model_id, cancel_rx, permit,
        )
        .await;
    });
}

/// 中继下载进度直到结束：每条进度更新 downloads 表并广播 WS；
/// 结束后把 entry 置为 completed / failed / cancelled 并发送最后一条 WS 消息。
///
/// 两阶段结构（P2-6 取消所需）：ep-core `DownloadHandle::wait()` 会消费句柄，
/// 消费后无法再调 `cancel()`，故：
/// - **阶段 A**：句柄存活，select 中继进度事件 / 响应取消信号 / 等待终态事件；
///   收到取消信号时调用 `handle.cancel()`（kill python 子进程）后转入阶段 B；
///   进度事件 Lagged（可能丢失终态事件）同样转阶段 B 兜底。
/// - **阶段 B**：`handle.wait()` 取权威终态，随后抽干剩余进度事件补中继。
///
/// std Mutex 仅覆盖表更新，短临界区、不跨 await；广播发送失败一律忽略。
/// 终态落定后从 [`DOWNLOAD_CANCEL_TXS`] 移除取消信号（顺序保证：移除前
/// entry 已是终态，取消端点不会误判"下载中"）。
#[allow(clippy::too_many_arguments)]
async fn monitor_download(
    mut handle: ep_core::model::DownloadHandle,
    downloads: Arc<std::sync::Mutex<std::collections::HashMap<String, DownloadEntry>>>,
    ws_tx: tokio::sync::broadcast::Sender<WsMessage>,
    key: String,
    module_id: String,
    model_id: String,
    cancel_rx: oneshot::Receiver<()>,
    permit: OwnedSemaphorePermit,
) {
    let mut rx = handle.subscribe_progress();

    let mut last: Option<DownloadProgress> = None;
    let mut cancel_rx: Option<oneshot::Receiver<()>> = Some(cancel_rx);

    // 阶段 A：中继进度直至终态事件 / 取消信号 / Lagged 兜底
    loop {
        tokio::select! {
            // 取消信号到达即 break（break 后不再轮询该分支，无需 fuse）
            _ = async {
                match cancel_rx.as_mut() {
                    Some(rx) => {
                        let _ = rx.await;
                    }
                    None => std::future::pending::<()>().await,
                }
            } => {
                info!(module_id = %module_id, model_id = %model_id, "API: relaying cancel signal to download supervisor");
                handle.cancel();
                break;
            }
            recv = rx.recv() => match recv {
                Ok(p) => {
                    let terminal = matches!(
                        p.state,
                        DownloadState::Completed | DownloadState::Failed(_) | DownloadState::Cancelled
                    );
                    relay_download_progress(&downloads, &ws_tx, &key, &p);
                    last = Some(p);
                    if terminal {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    // 可能丢失终态事件 → 转阶段 B 用 wait() 取权威终态
                    debug!(module_id = %module_id, model_id = %model_id, lagged = n, "download progress events lagged, falling back to wait()");
                    break;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            },
        }
    }

    // 阶段 B：权威终态（wait 消费句柄）+ 抽干剩余进度事件（含终态前最后入队的事件）
    let wait_result = handle.wait().await;
    while let Ok(p) = rx.try_recv() {
        relay_download_progress(&downloads, &ws_tx, &key, &p);
        last = Some(p);
    }

    // 终态判定：优先监督任务的终态进度事件，异常路径回退 wait 返回值
    // （ep-core 监督任务的取消消息为英文 "download cancelled"，两种文案都识别）
    let (percent_keep, bytes_keep) = last
        .as_ref()
        .map(|p| (p.percent, p.bytes))
        .unwrap_or((0.0, 0));
    let (final_state, final_percent, final_bytes) = match last.as_ref().map(|p| &p.state) {
        Some(DownloadState::Completed) => ("completed", 100.0, bytes_keep),
        Some(DownloadState::Failed(_)) => ("failed", percent_keep, bytes_keep),
        Some(DownloadState::Cancelled) => ("cancelled", percent_keep, bytes_keep),
        _ => match wait_result {
            Ok(bytes) => ("completed", 100.0, bytes),
            Err(msg) if msg.contains("取消") || msg.contains("cancelled") => {
                ("cancelled", percent_keep, bytes_keep)
            }
            Err(err) => {
                warn!(module_id = %module_id, model_id = %model_id, error = %err, "model download failed");
                ("failed", percent_keep, bytes_keep)
            }
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
    // 终态落定后才移除取消信号（见函数头注释的顺序保证）
    {
        let mut guard = DOWNLOAD_CANCEL_TXS.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(txs) = guard.as_mut() {
            txs.remove(&key);
        }
    }
    info!(
        module_id = %module_id,
        model_id = %model_id,
        state = final_state,
        bytes = final_bytes,
        "model download finished"
    );
    drop(permit); // 归还并发闸空位（P2-1）
}

/// GET /api/models/downloads — 全部下载记录（按 started_at 升序）。
///
/// 响应体为数组，元素即 `DownloadEntry` 的蛇形命名字段（前端契约 ModelDownloadStatus[]）。
/// `state` 取值：`queued`（并发闸排队，B6 新增）/ `downloading` / `completed` /
/// `failed` / `cancelled`。
async fn list_downloads(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    let now = chrono::Utc::now();
    let mut entries: Vec<DownloadEntry> = {
        let mut map = state.downloads.lock().unwrap_or_else(|e| e.into_inner());
        // P2 修复：终态条目 TTL 淘汰（默认 1h）——completed/failed/cancelled
        // 超过 TTL 后移除，防 downloads 表无限增长；活跃（queued/downloading）
        // 条目不淘汰。started_at 无法解析视为终态已过期（防御性淘汰）。
        let expired: Vec<String> = map
            .iter()
            .filter(|(_, e)| is_terminal_download_state(&e.state))
            .filter(|(_, e)| {
                chrono::DateTime::parse_from_rfc3339(&e.started_at)
                    .map(|t| {
                        (now - t.with_timezone(&chrono::Utc)).num_seconds() > DOWNLOADS_TTL_SECS
                    })
                    .unwrap_or(true)
            })
            .map(|(k, _)| k.clone())
            .collect();
        for k in expired {
            map.remove(&k);
        }
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
    // std MutexGuard 不得跨 await（!Send）：先在短临界区内取标志，再构造响应。
    // 排队中（queued）同样保护：否则排队任务启动后会写入已删除的目录。
    let downloading = {
        let map = state.downloads.lock().unwrap_or_else(|e| e.into_inner());
        map.get(&key).is_some_and(|e| is_active_download(&e.state))
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

// ─── Wave 2（B6）：tags 端点 + 下载取消 ─────────────────────────────────────

/// PUT /api/models/:module_id/:model_id/tags — 设置模型标签（§5.1 / §8.1）
///
/// 请求体 `{"tags": [...]}`：全量覆写语义，空数组 = 清空全部标签。
/// 标签写入模型目录的 `.ep_meta.json`（随整合包流转，§5.1）。
///
/// 归一化：逐项 trim、去空、保序去重。
///
/// 状态码：200 成功；404 模块/模型不存在或模型无 `.ep_meta.json`
/// （未下载/导入，或 meta 被手动删除）；500 meta 写入失败。
async fn set_model_tags(
    State(state): State<Arc<AppState>>,
    Path((module_id, model_id)): Path<(String, String)>,
    Json(req): Json<TagsRequest>,
) -> (StatusCode, Json<Value>) {
    let decl = match find_model_decl(&state, &module_id, &model_id).await {
        Ok(d) => d,
        Err(resp) => return resp,
    };

    let mgr = build_model_manager(&state).await;
    let mut meta = match mgr.read_meta(&decl.target_dir) {
        Some(m) => m,
        None => {
            return err_response(
                &state,
                StatusCode::NOT_FOUND,
                "apiModels.tagsNoMeta",
                &[],
            )
            .await;
        }
    };

    // 归一化：trim、去空、保序去重
    let mut tags: Vec<String> = Vec::new();
    for raw in req.tags {
        let tag = raw.trim().to_string();
        if tag.is_empty() || tags.contains(&tag) {
            continue;
        }
        tags.push(tag);
    }
    meta.tags = tags.clone();

    if let Err(e) = mgr.write_meta(&decl.target_dir, &meta) {
        warn!(module_id = %module_id, model_id = %model_id, error = %e, "failed to write model meta tags");
        return err_response(
            &state,
            StatusCode::INTERNAL_SERVER_ERROR,
            "apiModels.tagsWriteFailed",
            &[("detail", e.to_string())],
        )
        .await;
    }

    info!(module_id = %module_id, model_id = %model_id, tags = ?tags, "API: model tags updated");
    (
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "module_id": module_id,
            "model_id": model_id,
            "tags": tags,
        })),
    )
}

/// POST /api/models/:module_id/:model_id/cancel-download — 取消模型下载（P2-6）
///
/// - 200：取消指令已发出。排队中（`queued`）→ 直接标记 `cancelled`（排队任务
///   取得闸门空位时检测到终态自行退出）；下载中（`downloading`）→ 经
///   ep-core `DownloadHandle::cancel()` kill python 子进程，监督任务落
///   `cancelled` 终态并广播 WS。**200 表示"取消已受理"**，最终状态以
///   downloads 表 / WS 为准（极端竞态下下载可能恰好抢先完成）。
/// - 409：无进行中的下载（无记录，或记录已是 completed/failed/cancelled 终态）。
/// - 404：模块或模型不存在。
async fn cancel_model_download(
    State(state): State<Arc<AppState>>,
    Path((module_id, model_id)): Path<(String, String)>,
) -> (StatusCode, Json<Value>) {
    if let Err(resp) = find_model_decl(&state, &module_id, &model_id).await {
        return resp;
    }
    let key = download_key(&module_id, &model_id);

    // 短临界区内定位目标并原子处理 queued 分支：
    // queued → 直接置 cancelled（排队任务复查时自行退出，不会拉活）
    enum Target {
        Queued,
        Downloading,
        None,
    }
    let target = {
        let mut map = state.downloads.lock().unwrap_or_else(|e| e.into_inner());
        match map.get_mut(&key) {
            Some(e) if e.state == "queued" => {
                e.state = "cancelled".to_string();
                Target::Queued
            }
            Some(e) if e.state == "downloading" => Target::Downloading,
            _ => Target::None,
        }
    };

    match target {
        Target::None => {
            err_response(
                &state,
                StatusCode::CONFLICT,
                "apiModels.cancelNotActive",
                &[],
            )
            .await
        }
        Target::Queued => {
            let _ = state.model_download_tx.send(WsMessage::ModelDownload {
                module_id: module_id.clone(),
                model_id: model_id.clone(),
                percent: 0.0,
                state: "cancelled".to_string(),
                bytes: 0,
            });
            info!(module_id = %module_id, model_id = %model_id, "API: queued model download cancelled");
            (StatusCode::OK, Json(json!({ "ok": true })))
        }
        Target::Downloading => {
            let tx = {
                let mut guard = DOWNLOAD_CANCEL_TXS.lock().unwrap_or_else(|e| e.into_inner());
                guard.as_mut().and_then(|txs| txs.remove(&key))
            };
            match tx {
                Some(tx) => {
                    if tx.send(()).is_ok() {
                        info!(module_id = %module_id, model_id = %model_id, "API: model download cancel signal sent");
                        return (StatusCode::OK, Json(json!({ "ok": true })));
                    }
                    // 监督任务已退出（下载恰好结束）→ 无可取消，落 409
                    err_response(
                        &state,
                        StatusCode::CONFLICT,
                        "apiModels.cancelNotActive",
                        &[],
                    )
                    .await
                }
                // 注册表与 downloads 表同锁序更新，走到这里说明监督任务已
                // 落终态并移除登记 → 无可取消
                None => {
                    err_response(
                        &state,
                        StatusCode::CONFLICT,
                        "apiModels.cancelNotActive",
                        &[],
                    )
                    .await
                }
            }
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::body::Body;
    use axum::http::{Method, Request};
    use ep_core::config::AppConfig;
    use ep_core::module::discovery::{DiscoveredModule, DiscoveryStatus};
    use ep_core::module::manifest::{
        ComputeConfig, InterfaceConfig, InterfaceType, ModuleInfo, RuntimeConfig, RuntimeType,
    };
    use ep_core::port::PortManager;
    use ep_core::types::{ComputeBackend, ModuleCategory};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

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
            qualified_id: None,
            vram_estimate_mb: None,
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
            qualified_id: None,
            vram_estimate_mb: None,
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
        // started_at 用近期时刻（TTL 淘汰边界内），保持排序断言稳定
        let now = chrono::Utc::now();
        let t_a = (now - chrono::Duration::seconds(120)).to_rfc3339();
        let t_b = (now - chrono::Duration::seconds(60)).to_rfc3339();
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
                    started_at: t_b,
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
                    started_at: t_a,
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

    // ── downloads TTL 淘汰（P2：终态条目不无限堆积） ──────────────────────

    #[tokio::test]
    async fn test_list_downloads_evicts_expired_terminal_entries() {
        let state = test_state();
        let now = chrono::Utc::now();
        let insert = |state: &AppState, key: &str, mod_id: &str, model_id: &str,
                      dl_state: &str, started_at: &str| {
            state.downloads.lock().unwrap().insert(
                key.into(),
                DownloadEntry {
                    module_id: mod_id.into(),
                    model_id: model_id.into(),
                    source: "huggingface".into(),
                    percent: 10.0,
                    bytes: 7,
                    state: dl_state.into(),
                    started_at: started_at.into(),
                },
            );
        };
        // 过期终态（>1h）：completed 3h 前 / failed 2h 前 → 应被淘汰
        insert(
            &state,
            "m1:old-completed",
            "m1",
            "old-completed",
            "completed",
            &(now - chrono::Duration::hours(3)).to_rfc3339(),
        );
        insert(
            &state,
            "m1:old-failed",
            "m1",
            "old-failed",
            "failed",
            &(now - chrono::Duration::hours(2)).to_rfc3339(),
        );
        // 新鲜终态（1min 前）→ 保留；活跃条目 → 永不淘汰
        insert(
            &state,
            "m1:recent-cancelled",
            "m1",
            "recent-cancelled",
            "cancelled",
            &(now - chrono::Duration::seconds(60)).to_rfc3339(),
        );
        insert(
            &state,
            "m1:active-downloading",
            "m1",
            "active-downloading",
            "downloading",
            &now.to_rfc3339(),
        );

        let (status, json) = list_downloads(State(state.clone())).await;
        assert_eq!(status, StatusCode::OK);
        let arr = json.0.as_array().unwrap();
        let states: Vec<&str> = arr
            .iter()
            .map(|v| v["state"].as_str().unwrap())
            .collect();
        assert!(!states.contains(&"completed"), "过期 completed 应被淘汰");
        assert!(!states.contains(&"failed"), "过期 failed 应被淘汰");
        assert!(states.contains(&"cancelled"), "新鲜终态应保留");
        assert!(states.contains(&"downloading"), "活跃条目不淘汰");
        // 淘汰落盘：表内只剩 2 条
        assert_eq!(state.downloads.lock().unwrap().len(), 2);
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

    #[tokio::test]
    async fn test_check_update_manual_unaffected_by_check_updates_switch() {
        // P1-10 接线语义：check_updates 只控制后台自动/定时检查；
        // 用户手动触发的 check-update 端点不受开关约束（false 时照常可用）
        let state = test_state();
        state.config.write().await.general.check_updates = false;

        let (status, json) = check_model_update(
            State(state.clone()),
            Path((MODULE_ID.to_string(), URL_MODEL_ID.to_string())),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json.0["available"], false);
        assert_eq!(json.0["reason"], "URL 来源不支持更新检查");

        let (status, json) = check_model_update(
            State(state),
            Path((MODULE_ID.to_string(), MODEL_ID.to_string())),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
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

    // ── Wave 2（B6）：Router::oneshot 设施 ────────────────────────────────

    /// 挂载完整 /api 路由树（同 packs.rs 测试惯例）
    fn api_app(state: Arc<AppState>) -> Router {
        crate::api::api_router(state.clone()).with_state(state)
    }

    fn get_request(uri: &str) -> Request<Body> {
        Request::builder().uri(uri).body(Body::empty()).unwrap()
    }

    fn json_request(method: Method, uri: &str, body: Value) -> Request<Body> {
        Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap()
    }

    async fn body_json(resp: axum::response::Response) -> Value {
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// 在模型目录放置文件 + `.ep_meta.json`（ModelMeta 全字段字面量，双平台 Path::join）
    fn seed_model_with_meta(
        state: &AppState,
        source: &str,
        tags: Vec<&str>,
        qualified_id: Option<&str>,
        pack_id: Option<&str>,
    ) {
        let dir = state.root.join("models").join(TARGET_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("model.bin"), b"weights").unwrap();
        let meta = ep_core::model::ModelMeta {
            module_id: MODULE_ID.to_string(),
            model_id: MODEL_ID.to_string(),
            source: source.to_string(),
            repo_id: String::new(),
            revision: String::new(),
            downloaded_at: "2026-08-04T00:00:00+00:00".to_string(),
            total_size_bytes: 7,
            qualified_id: qualified_id.map(str::to_string),
            tags: tags.into_iter().map(str::to_string).collect(),
            pack_id: pack_id.map(str::to_string),
        };
        std::fs::write(
            dir.join(".ep_meta.json"),
            serde_json::to_vec(&meta).unwrap(),
        )
        .unwrap();
    }

    /// 创建假 venv python：unix 为可执行 shell 脚本（立即成功退出）；
    /// 其他平台为占位文件（通过 exists() 检查，spawn 必失败 → 下载落 failed，
    /// 用于不依赖真实解释器验证排队/启动流转）。
    fn create_fake_venv_python(state: &AppState) -> PathBuf {
        create_fake_venv_python_with(state, "#!/bin/sh\nsleep 0.2\nexit 0\n")
    }

    #[cfg(unix)]
    fn create_fake_venv_python_with(state: &AppState, script: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = venv_python_path(&state.root, MODULE_ID);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[cfg(not(unix))]
    fn create_fake_venv_python_with(state: &AppState, _script: &str) -> PathBuf {
        let path = venv_python_path(&state.root, MODULE_ID);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"not a real executable").unwrap();
        path
    }

    /// 占住下载并发闸的 permit，模拟 N 个下载在途（默认配置上限为 2）。
    /// 持有者 drop permit 即归还；测试进程共享同一闸门实例，持有时长尽量短。
    async fn occupy_gate(count: usize) -> Vec<OwnedSemaphorePermit> {
        let gate = download_gate(2);
        let mut permits = Vec::new();
        for _ in 0..count {
            permits.push(gate.clone().acquire_owned().await.unwrap());
        }
        permits
    }

    // ── tags 端点（§8.1）：写后 GET 可见的往返 ───────────────────────────

    #[tokio::test]
    async fn test_tags_roundtrip_via_router() {
        let state = test_state();
        seed_model_with_meta(&state, "huggingface", vec![], None, None);
        let app = api_app(state.clone());

        // PUT 写入（含归一化：trim / 去空 / 保序去重）
        let resp = app
            .clone()
            .oneshot(json_request(
                Method::PUT,
                &format!("/models/{MODULE_ID}/{MODEL_ID}/tags"),
                json!({ "tags": ["字幕", " 视频 ", "字幕", ""] }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["ok"], true);
        assert_eq!(body["tags"], json!(["字幕", "视频"]));

        // GET 模块详情：tags 写后可见
        let resp = app
            .clone()
            .oneshot(get_request(&format!("/models/{MODULE_ID}")))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["models"][0]["tags"], json!(["字幕", "视频"]));

        // GET 全局列表：tags 同样透传
        let resp = app.oneshot(get_request("/models")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["modules"][0]["models"][0]["tags"], json!(["字幕", "视频"]));
    }

    #[tokio::test]
    async fn test_tags_put_empty_array_clears() {
        let state = test_state();
        seed_model_with_meta(
            &state,
            "pack",
            vec!["字幕"],
            Some("ep.systran.faster-whisper"),
            Some("pigeonfish.subtitle-kit"),
        );
        let app = api_app(state.clone());

        let resp = app
            .oneshot(json_request(
                Method::PUT,
                &format!("/models/{MODULE_ID}/{MODEL_ID}/tags"),
                json!({ "tags": [] }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["tags"], json!([]));

        // meta 文件确实被清空（空 Vec 序列化时按 skip_serializing_if 省略键）
        let mgr = {
            let cfg = state.config.read().await;
            ModelManager::new(&cfg.models, &state.root)
        };
        let meta = mgr.read_meta(TARGET_DIR).expect("meta must remain readable");
        assert!(meta.tags.is_empty());
        // 其余字段不被 tags 更新破坏
        assert_eq!(meta.pack_id.as_deref(), Some("pigeonfish.subtitle-kit"));
        assert_eq!(
            meta.qualified_id.as_deref(),
            Some("ep.systran.faster-whisper")
        );
    }

    #[tokio::test]
    async fn test_tags_put_no_meta_404() {
        let state = test_state();
        let app = api_app(state);
        let resp = app
            .oneshot(json_request(
                Method::PUT,
                &format!("/models/{MODULE_ID}/{MODEL_ID}/tags"),
                json!({ "tags": ["a"] }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let body = body_json(resp).await;
        // C8 已落盘：断言真实 zh 文案（默认语言 zh-CN）
        assert_eq!(body["error"], "模型没有下载元数据（.ep_meta.json），无法设置标签");
    }

    #[tokio::test]
    async fn test_tags_put_unknown_model_404() {
        let state = test_state();
        let app = api_app(state);
        let resp = app
            .oneshot(json_request(
                Method::PUT,
                &format!("/models/{MODULE_ID}/ghost-model/tags"),
                json!({ "tags": ["a"] }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let body = body_json(resp).await;
        assert!(body["error"].as_str().unwrap().contains("不存在"));
    }

    #[tokio::test]
    async fn test_tags_put_invalid_body_rejected() {
        let state = test_state();
        let app = api_app(state);
        // 缺 tags 字段 → axum Json 提取器拒绝（JsonDataError → 422）
        let resp = app
            .oneshot(json_request(
                Method::PUT,
                &format!("/models/{MODULE_ID}/{MODEL_ID}/tags"),
                json!({}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    // ── pack 来源展示：列表响应透传 meta 字段（§5.1） ────────────────────

    #[tokio::test]
    async fn test_pack_fields_passthrough_in_list_responses() {
        let state = test_state();
        seed_model_with_meta(
            &state,
            "pack",
            vec!["字幕", "视频"],
            Some("ep.systran.faster-whisper"),
            Some("pigeonfish.subtitle-kit"),
        );
        let app = api_app(state);

        // GET /api/models：source="pack" 不做特殊处理（source 仍取声明），
        // pack 来源经 pack_id/qualified_id/tags 透传
        let resp = app.clone().oneshot(get_request("/models")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        let m1 = &body["modules"][0]["models"][0];
        assert_eq!(m1["source"], "huggingface");
        assert_eq!(m1["pack_id"], "pigeonfish.subtitle-kit");
        assert_eq!(m1["qualified_id"], "ep.systran.faster-whisper");
        assert_eq!(m1["tags"], json!(["字幕", "视频"]));
        // 无 meta 的模型（m2 未下载）：pack_id=null / qualified_id=null / tags=[]
        let m2 = &body["modules"][0]["models"][1];
        assert_eq!(m2["pack_id"], Value::Null);
        assert_eq!(m2["qualified_id"], Value::Null);
        assert_eq!(m2["tags"], json!([]));

        // GET /api/models/{m}：同形状透传
        let resp = app
            .oneshot(get_request(&format!("/models/{MODULE_ID}")))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        let m1 = &body["models"][0];
        assert_eq!(m1["pack_id"], "pigeonfish.subtitle-kit");
        assert_eq!(m1["qualified_id"], "ep.systran.faster-whisper");
        assert_eq!(m1["tags"], json!(["字幕", "视频"]));
    }

    // ── cancel-download（P2-6） ──────────────────────────────────────────

    #[tokio::test]
    async fn test_cancel_download_no_active_409() {
        let state = test_state();
        // 无下载记录 → 409
        let app = api_app(state.clone());
        let resp = app
            .oneshot(json_request(
                Method::POST,
                &format!("/models/{MODULE_ID}/{MODEL_ID}/cancel-download"),
                json!({}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        let body = body_json(resp).await;
        assert_eq!(body["error"], "该模型没有进行中的下载，无需取消");

        // 终态记录（completed）同样视为"无进行中下载" → 409
        seed_entry(&state, "completed", "2026-08-04T00:00:00+00:00");
        let app2 = api_app(state);
        let resp = app2
            .oneshot(json_request(
                Method::POST,
                &format!("/models/{MODULE_ID}/{MODEL_ID}/cancel-download"),
                json!({}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn test_cancel_download_unknown_model_404() {
        let state = test_state();
        let app = api_app(state);
        let resp = app
            .oneshot(json_request(
                Method::POST,
                &format!("/models/{MODULE_ID}/ghost-model/cancel-download"),
                json!({}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_cancel_queued_download() {
        let state = test_state();
        create_fake_venv_python(&state);
        let mut rx = state.model_download_tx.subscribe();

        // 闸门占满 → 下载落 queued
        let holds = occupy_gate(2).await;
        let app = api_app(state.clone());
        let resp = app
            .oneshot(json_request(
                Method::POST,
                &format!("/models/{MODULE_ID}/download"),
                json!({ "model_id": MODEL_ID }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
        let body = body_json(resp).await;
        assert_eq!(body["ok"], true);
        assert_eq!(body["queued"], true);
        {
            let map = state.downloads.lock().unwrap();
            assert_eq!(
                map.get(&download_key(MODULE_ID, MODEL_ID)).unwrap().state,
                "queued"
            );
        }

        // 取消排队中的下载 → 200 + entry cancelled + WS 广播
        let app2 = api_app(state.clone());
        let resp = app2
            .oneshot(json_request(
                Method::POST,
                &format!("/models/{MODULE_ID}/{MODEL_ID}/cancel-download"),
                json!({}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        {
            let map = state.downloads.lock().unwrap();
            assert_eq!(
                map.get(&download_key(MODULE_ID, MODEL_ID)).unwrap().state,
                "cancelled"
            );
        }
        let mut saw_cancelled = false;
        while let Ok(Ok(msg)) =
            tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv()).await
        {
            if let WsMessage::ModelDownload { state: s, .. } = msg {
                if s == "cancelled" {
                    saw_cancelled = true;
                    break;
                }
            }
        }
        assert!(saw_cancelled, "expected a 'cancelled' WS message");

        // 释放闸门：排队任务取得空位后检测终态自行退出，不得拉活已取消的下载
        drop(holds);
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let map = state.downloads.lock().unwrap();
        assert_eq!(
            map.get(&download_key(MODULE_ID, MODEL_ID)).unwrap().state,
            "cancelled"
        );
    }

    /// 下载中的取消需要真实子进程可 kill，仅 unix 假 python 可验证
    #[tokio::test]
    #[cfg(unix)]
    async fn test_cancel_active_download_unix() {
        let state = test_state();
        // 慢速假 python：保证取消时下载仍在进行中
        create_fake_venv_python_with(&state, "#!/bin/sh\nsleep 30\nexit 0\n");

        let (status, body) = download_model(
            State(state.clone()),
            Path(MODULE_ID.to_string()),
            Some(Json(json!({ "model_id": MODEL_ID }))),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(body.0["queued"], false); // 闸门有空位 → 立即启动
        {
            let map = state.downloads.lock().unwrap();
            assert_eq!(
                map.get(&download_key(MODULE_ID, MODEL_ID)).unwrap().state,
                "downloading"
            );
        }

        // 取消进行中的下载 → 200（经 DownloadHandle::cancel kill 子进程）
        let app = api_app(state.clone());
        let resp = app
            .oneshot(json_request(
                Method::POST,
                &format!("/models/{MODULE_ID}/{MODEL_ID}/cancel-download"),
                json!({}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // entry 落 cancelled 终态 + 闸门归还
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut cancelled = false;
        while tokio::time::Instant::now() < deadline && !cancelled {
            let st = state
                .downloads
                .lock()
                .unwrap()
                .get(&download_key(MODULE_ID, MODEL_ID))
                .map(|e| e.state.clone());
            cancelled = st.as_deref() == Some("cancelled");
            if !cancelled {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        }
        assert!(cancelled, "download entry must reach 'cancelled'");
    }

    // ── 下载并发闸（P2-1）：超量下载排队 ─────────────────────────────────

    #[tokio::test]
    async fn test_download_gate_queues_then_starts_when_freed() {
        let state = test_state();
        create_fake_venv_python(&state);

        // 占满闸门（模拟 max_concurrent_downloads=2 且 2 个下载在途）
        let holds = occupy_gate(2).await;

        // 超量下载：仍 202，但 queued=true 且 entry 落 queued
        let app = api_app(state.clone());
        let resp = app
            .oneshot(json_request(
                Method::POST,
                &format!("/models/{MODULE_ID}/download"),
                json!({ "model_id": MODEL_ID }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
        let body = body_json(resp).await;
        assert_eq!(body["ok"], true);
        assert_eq!(body["queued"], true);
        {
            let map = state.downloads.lock().unwrap();
            assert_eq!(
                map.get(&download_key(MODULE_ID, MODEL_ID)).unwrap().state,
                "queued"
            );
        }

        // 持闸期间保持排队
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        {
            let map = state.downloads.lock().unwrap();
            assert_eq!(
                map.get(&download_key(MODULE_ID, MODEL_ID)).unwrap().state,
                "queued"
            );
        }

        // 释放闸门 → 排队任务取得空位启动，离开 queued 状态
        // （unix 假 python → 最终 completed；其他平台占位文件 spawn 失败 → failed）
        drop(holds);
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let st = state
                .downloads
                .lock()
                .unwrap()
                .get(&download_key(MODULE_ID, MODEL_ID))
                .map(|e| e.state.clone());
            if st.as_deref().is_some_and(|s| s != "queued") {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "queued download did not start after gate freed"
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }

    #[tokio::test]
    async fn test_download_duplicate_queued_entry_409() {
        // 排队中（queued）与下载中同属活跃下载，重复提交一律 409
        let state = test_state();
        seed_entry(&state, "queued", "2026-08-04T00:00:00+00:00");
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
    async fn test_delete_while_queued_409() {
        // 排队中的下载同样受删除保护（否则排队任务启动后写入已删除目录）
        let state = test_state();
        seed_entry(&state, "queued", "2026-08-04T00:00:00+00:00");
        let (status, json) = delete_model(
            State(state),
            Path((MODULE_ID.to_string(), MODEL_ID.to_string())),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(json.0["error"].as_str().unwrap().contains("正在下载"));
    }
}