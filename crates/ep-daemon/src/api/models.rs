//! 模型管理 API — 状态查询、本地导入
//!
//! 不实现自动下载，由用户决定是否导入或下载。

use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    Router,
    extract::{Path, State},
    routing::{get, post},
    Json,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::{info, warn};

use ep_core::model::{ModelInfo, ModelManager, ModelStatus};

use crate::state::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/models", get(list_models))
        .route("/models/{module_id}", get(module_models))
        .route("/models/{module_id}/import", post(import_model))
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
async fn build_model_manager(state: &AppState) -> ModelManager {
    let config = state.config.read().await;
    let root = std::env::current_dir().unwrap_or_default();
    ModelManager::new(&config.models, &root)
}

/// 查找模块的 manifest（通过 module_id）
async fn find_module_manifest(
    state: &AppState,
    module_id: &str,
) -> Option<ep_core::module::manifest::ModuleManifest> {
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
                    "source": format!("{:?}", model.source),
                    "size_estimate_mb": model.size_estimate_mb,
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
) -> Json<Value> {
    let manifest = match find_module_manifest(&state, &module_id).await {
        Some(mf) => mf,
        None => {
            return Json(json!({
                "error": format!("module '{module_id}' not found")
            }));
        }
    };

    if manifest.models.is_empty() {
        return Json(json!({
            "module_id": module_id,
            "models": [],
            "message": "this module has no model declarations"
        }));
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
            })
        })
        .collect();

    Json(json!({
        "module_id": module_id,
        "module_name": manifest.module.name,
        "models": models,
    }))
}

/// POST /api/models/:module_id/import — 从本地路径导入模型
async fn import_model(
    State(state): State<Arc<AppState>>,
    Path(module_id): Path<String>,
    Json(req): Json<ImportRequest>,
) -> Json<Value> {
    let manifest = match find_module_manifest(&state, &module_id).await {
        Some(mf) => mf,
        None => {
            return Json(json!({
                "error": format!("module '{module_id}' not found")
            }));
        }
    };

    // 验证 model_id 存在于 manifest 中
    let model_decl = match manifest.models.iter().find(|m| m.id == req.model_id) {
        Some(m) => m,
        None => {
            let available: Vec<&str> = manifest.models.iter().map(|m| m.id.as_str()).collect();
            return Json(json!({
                "error": format!(
                    "model '{}' not found in module '{}'. Available models: {:?}",
                    req.model_id, module_id, available
                )
            }));
        }
    };

    let source_path = PathBuf::from(&req.source_path);
    if !source_path.is_dir() {
        return Json(json!({
            "error": format!(
                "source path '{}' does not exist or is not a directory",
                req.source_path
            )
        }));
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
            return Json(json!({
                "error": format!("failed to create target directory: {e}")
            }));
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

            Json(json!({
                "status": "imported",
                "module_id": module_id,
                "model_id": req.model_id,
                "target_dir": model_decl.target_dir,
                "file_count": file_count,
                "total_bytes": total_bytes,
            }))
        }
        Err(e) => {
            warn!(error = %e, "model import failed");
            Json(json!({
                "error": format!("import failed: {e}")
            }))
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