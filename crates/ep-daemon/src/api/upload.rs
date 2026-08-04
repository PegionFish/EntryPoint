//! 模型上传 API — 浏览器端把本地模型文件夹/压缩包上传到服务器
//!
//! ⚠ 文件所有权：Wave 2 上传代理。
//!
//! 请求契约（前端冻结，multipart/form-data）：
//! - `model_id`：字符串，模块清单中的模型 ID（[[models]].id）
//! - `files`：可重复，文件块（逐 chunk 流式写入暂存目录，不整块进内存）
//! - `paths`：可重复字符串，与 `files` 同序，为文件相对路径（webkitRelativePath）
//!
//! 归档模式：仅一个文件且文件名以 .zip/.tar.gz/.tgz 结尾、`paths` 为空或仅一项 →
//! 服务端解包（逐条目防 zip-slip）；压缩包内只有一个顶层目录时剥掉一层作为模型根。
//!
//! Body 限制：模型可达数 GB，本路由通过 `DefaultBodyLimit::disable()` 关闭 axum
//! 默认 2MB body 上限。layer 加在本文件 `router()` 内的路由之后，merge 进
//! api_router 后只作用于上传路由，无需改动 mod.rs。

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::multipart::MultipartRejection;
use axum::extract::{DefaultBodyLimit, Multipart, Path as UrlPath, State};
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;
use tracing::{debug, info, warn};

use ep_core::model::{dir_total_size, ModelManager, ModelMeta};
use ep_core::module::manifest::{ModelDecl, ModelSource, ModuleManifest};

use super::err_response;
use crate::state::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/models/{module_id}/upload", post(upload_model))
        // 直跑输入上传（§8.1）：输入文件同样可能很大（音视频），同享 body 上限豁免
        .route("/upload/input", post(upload_input))
        // 模型文件可达数 GB：仅对本文件路由关闭默认 2MB body 上限
        .layer(DefaultBodyLimit::disable())
}

/// 内部错误表示：HTTP 状态码 + i18n 键 + 插值参数
///
/// 最终文案由 `err_response` 按请求语言（config.general.language）本地化，
/// 键定义见 `i18n/locales/*/apiModels.json`。
struct UploadError {
    status: StatusCode,
    key: &'static str,
    params: Vec<(&'static str, String)>,
}

fn err(status: StatusCode, key: &'static str) -> UploadError {
    UploadError {
        status,
        key,
        params: Vec::new(),
    }
}

fn err_with(
    status: StatusCode,
    key: &'static str,
    params: Vec<(&'static str, String)>,
) -> UploadError {
    UploadError {
        status,
        key,
        params,
    }
}

/// 便捷构造：携带单个 {{detail}} 插值（通常为底层系统错误）
fn err_detail(
    status: StatusCode,
    key: &'static str,
    detail: impl std::fmt::Display,
) -> UploadError {
    err_with(status, key, vec![("detail", detail.to_string())])
}

/// 归档解包错误：i18n 键 + 插值参数（跨 spawn_blocking 边界后转为 UploadError）
#[derive(Debug)]
struct ExtractError {
    key: &'static str,
    params: Vec<(&'static str, String)>,
}

impl ExtractError {
    fn detail(key: &'static str, detail: impl std::fmt::Display) -> Self {
        Self {
            key,
            params: vec![("detail", detail.to_string())],
        }
    }

    fn entry(key: &'static str, entry: impl std::fmt::Display) -> Self {
        Self {
            key,
            params: vec![("entry", entry.to_string())],
        }
    }
}

/// 解包错误统一按 400（用户提供的归档内容有问题）返回
impl From<ExtractError> for UploadError {
    fn from(xe: ExtractError) -> Self {
        UploadError {
            status: StatusCode::BAD_REQUEST,
            key: xe.key,
            params: xe.params,
        }
    }
}

/// 暂存区接收到的单个文件块
struct StagedFile {
    /// 暂存临时文件路径（staging/__parts/ 下）
    temp: PathBuf,
    /// 浏览器提供的文件名（content-disposition filename）
    file_name: Option<String>,
}

/// POST /api/models/:module_id/upload — 浏览器上传模型文件
async fn upload_model(
    State(state): State<Arc<AppState>>,
    UrlPath(module_id): UrlPath<String>,
    multipart: Result<Multipart, MultipartRejection>,
) -> (StatusCode, Json<Value>) {
    let multipart = match multipart {
        Ok(m) => m,
        Err(e) => {
            warn!(error = %e, "multipart request rejected");
            return err_response(
                &state,
                StatusCode::BAD_REQUEST,
                "apiModels.uploadMultipartInvalid",
                &[],
            )
            .await;
        }
    };

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

    let mgr = build_model_manager(&state).await;

    // 暂存目录：<模型根>/.upload-staging/<id>；无论成功失败，结束时一律清理
    let staging = mgr.cache_dir().join(".upload-staging").join(staging_id());
    if let Err(e) = tokio::fs::create_dir_all(&staging).await {
        return err_response(
            &state,
            StatusCode::INTERNAL_SERVER_ERROR,
            "apiModels.uploadStagingFailed",
            &[("detail", e.to_string())],
        )
        .await;
    }

    let result = handle_upload(&mgr, &manifest, &module_id, multipart, &staging).await;

    // defer 风格清理：成功后残留的解包产物 / 失败后的所有暂存内容
    if let Err(e) = tokio::fs::remove_dir_all(&staging).await {
        if e.kind() != std::io::ErrorKind::NotFound {
            warn!(staging = %staging.display(), error = %e, "failed to clean staging dir");
        }
    }
    // 尽力删除空的父目录（非空则报错忽略）
    let _ = tokio::fs::remove_dir(mgr.cache_dir().join(".upload-staging")).await;

    match result {
        Ok(body) => (StatusCode::OK, Json(body)),
        Err(ue) => err_response(&state, ue.status, ue.key, &ue.params).await,
    }
}

/// 上传主流程：解析 multipart → 暂存落盘 → 校验 → 落位 → 写 meta → 构造响应
async fn handle_upload(
    mgr: &ModelManager,
    manifest: &ModuleManifest,
    module_id: &str,
    mut multipart: Multipart,
    staging: &Path,
) -> Result<Value, UploadError> {
    // 临时文件放在独立子目录，避免与用户上传的相对路径撞名
    let parts_dir = staging.join("__parts");
    tokio::fs::create_dir_all(&parts_dir)
        .await
        .map_err(|e| {
            err_detail(
                StatusCode::INTERNAL_SERVER_ERROR,
                "apiModels.uploadStagingFailed",
                e,
            )
        })?;

    let mut model_id: Option<String> = None;
    let mut decl: Option<ModelDecl> = None;
    let mut staged: Vec<StagedFile> = Vec::new();
    let mut paths: Vec<String> = Vec::new();

    // ── 阶段 1：逐字段流式解析 ─────────────────────────────────────────────
    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|e| err_detail(StatusCode::BAD_REQUEST, "apiModels.uploadReadFailed", e))?
    {
        let name = field.name().map(str::to_string);
        match name.as_deref() {
            Some("model_id") => {
                let id = field
                    .text()
                    .await
                    .map_err(|e| {
                        err_detail(StatusCode::BAD_REQUEST, "apiModels.uploadReadFailed", e)
                    })?;
                // 以第一个 model_id 为准，尽早校验（失败时避免白传后续 GB 级文件）
                if model_id.is_none() {
                    match manifest.models.iter().find(|m| m.id == id) {
                        Some(d) => {
                            // 目标已存在且非空 → 尽早 409
                            let target = mgr.model_dir(&d.target_dir);
                            if target_blocked(&target).await {
                                return Err(err(StatusCode::CONFLICT, "apiModels.uploadConflict"));
                            }
                            decl = Some(d.clone());
                            model_id = Some(id);
                        }
                        None => {
                            return Err(err_with(
                                StatusCode::NOT_FOUND,
                                "apiModels.uploadModelNotFound",
                                vec![("model_id", id)],
                            ));
                        }
                    }
                }
            }
            Some("paths") => {
                let p = field
                    .text()
                    .await
                    .map_err(|e| {
                        err_detail(StatusCode::BAD_REQUEST, "apiModels.uploadReadFailed", e)
                    })?;
                paths.push(p);
            }
            Some("files") => {
                let file_name = field.file_name().map(str::to_string);
                let temp = parts_dir.join(format!("part-{:06}", staged.len()));
                let mut file =
                    tokio::fs::File::create(&temp).await.map_err(|e| {
                        err_detail(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "apiModels.uploadStagingFileFailed",
                            e,
                        )
                    })?;
                // 逐 chunk 流式写盘，绝不整块读入内存
                while let Some(chunk) = field
                    .chunk()
                    .await
                    .map_err(|e| {
                        err_detail(StatusCode::BAD_REQUEST, "apiModels.uploadReadFailed", e)
                    })?
                {
                    file.write_all(&chunk).await.map_err(|e| {
                        err_detail(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "apiModels.uploadWriteFailed",
                            e,
                        )
                    })?;
                }
                staged.push(StagedFile { temp, file_name });
            }
            _ => {
                // 未知字段：忽略（multer 在 next_field 时自动跳过未读内容）
            }
        }
    }

    // ── 阶段 2：请求完整性校验 ─────────────────────────────────────────────
    let model_id = model_id
        .ok_or_else(|| err(StatusCode::BAD_REQUEST, "apiModels.uploadMissingModelId"))?;
    let decl =
        decl.ok_or_else(|| err(StatusCode::BAD_REQUEST, "apiModels.uploadMissingModelId"))?;
    if staged.is_empty() {
        return Err(err(StatusCode::BAD_REQUEST, "apiModels.uploadNoFiles"));
    }
    if !paths.is_empty() && paths.len() != staged.len() {
        return Err(err_with(
            StatusCode::BAD_REQUEST,
            "apiModels.uploadPathsMismatch",
            vec![
                ("paths", paths.len().to_string()),
                ("files", staged.len().to_string()),
            ],
        ));
    }

    info!(
        module_id = %module_id,
        model_id = %model_id,
        file_count = staged.len(),
        "API: model upload started"
    );

    // ── 阶段 3：归档模式或文件夹模式落盘 ───────────────────────────────────
    let archive_kind = if staged.len() == 1 && paths.len() <= 1 {
        staged[0].file_name.as_deref().and_then(classify_archive)
    } else {
        None
    };

    let model_root: PathBuf = match archive_kind {
        Some(kind) => {
            // 归档模式：解包到 staging/__extract，再决定模型根（可能剥一层）
            let archive_path = staged.into_iter().next().expect("checked len == 1").temp;
            let extract_dir = staging.join("__extract");
            tokio::fs::create_dir_all(&extract_dir).await.map_err(|e| {
                err_detail(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "apiModels.uploadExtractDirFailed",
                    e,
                )
            })?;

            let src = archive_path.clone();
            let dst = extract_dir.clone();
            tokio::task::spawn_blocking(move || match kind {
                ArchiveKind::Zip => extract_zip(&src, &dst),
                ArchiveKind::TarGz => extract_tar_gz(&src, &dst),
            })
            .await
            .map_err(|e| {
                err_detail(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "apiModels.uploadExtractPanicked",
                    e,
                )
            })?
            .map_err(UploadError::from)?;

            // 解包完成立即删除压缩包副本，避免双倍占用磁盘
            let _ = tokio::fs::remove_file(&archive_path).await;

            // 压缩包内只有一个顶层目录 → 视为模型根（剥掉一层）
            single_top_dir(&extract_dir).await.unwrap_or(extract_dir)
        }
        None => {
            // 文件夹模式：按 paths（缺失则 file_name）把暂存文件布局进 staging
            layout_staged_files(staging, staged, &paths).await?;
            let _ = tokio::fs::remove_dir(&parts_dir).await; // 应为空，失败忽略
            staging.to_path_buf()
        }
    };

    // ── 阶段 4：落位（rename，跨设备回退递归复制）─────────────────────────
    let target = mgr.model_dir(&decl.target_dir);
    let total = finalize_staging(model_root.clone(), target.clone())
        .await
        .map_err(|fe| match fe {
            FinalizeError::Conflict => err(StatusCode::CONFLICT, "apiModels.uploadConflict"),
            FinalizeError::Empty => err(StatusCode::BAD_REQUEST, "apiModels.uploadEmpty"),
            FinalizeError::Other(key, detail) => {
                err_detail(StatusCode::INTERNAL_SERVER_ERROR, key, detail)
            }
        })?;

    // ── 阶段 5：写 .ep_meta.json ───────────────────────────────────────────
    // source 与现有 import handler 保持一致（上传按"本地导入"处理）；
    // repo_id：HF/MS 源填清单 repo_id，URL 源留空串。
    let meta = ModelMeta {
        module_id: module_id.to_string(),
        model_id: model_id.clone(),
        source: "local_import".to_string(),
        repo_id: match decl.source {
            ModelSource::Huggingface | ModelSource::Modelscope => {
                decl.repo_id.clone().unwrap_or_default()
            }
            ModelSource::Url => String::new(),
        },
        revision: String::new(),
        downloaded_at: chrono::Utc::now().to_rfc3339(),
        total_size_bytes: total,
        qualified_id: None,
        tags: vec![],
        pack_id: None,
    };
    if let Err(e) = mgr.write_meta(&decl.target_dir, &meta) {
        warn!(error = %e, "failed to write model meta after upload (non-fatal)");
    }

    info!(
        module_id = %module_id,
        model_id = %model_id,
        target_dir = %decl.target_dir,
        total_bytes = total,
        "API: model upload completed"
    );

    // ── 阶段 6：响应（ModelInfo，构造方式同 module_models handler）────────
    let infos = mgr.get_model_info(module_id, manifest);
    Ok(match infos.iter().find(|i| i.model_id == model_id) {
        Some(info) => json!({
            "model_id": info.model_id,
            "name": info.name,
            "target_dir": info.target_dir,
            "status": info.status.to_string(),
            "size_bytes": info.size_bytes,
            "file_count": info.file_count,
            "local_cache_path": info.local_cache_path,
        }),
        // 理论上不可达：decl 来自同一 manifest
        None => json!({
            "model_id": model_id,
            "target_dir": decl.target_dir,
            "status": "ready",
        }),
    })
}

// ─── 暂存布局 ───────────────────────────────────────────────────────────────

/// 把暂存文件按 paths[i]（或 file_name）移动到 staging 内的最终相对位置
async fn layout_staged_files(
    staging: &Path,
    staged: Vec<StagedFile>,
    paths: &[String],
) -> Result<(), UploadError> {
    let mut seen: HashSet<PathBuf> = HashSet::new();
    for (i, sf) in staged.into_iter().enumerate() {
        let raw: String = if paths.is_empty() {
            match sf.file_name.as_deref() {
                Some(n) if !n.is_empty() => n.to_string(),
                _ => {
                    return Err(err_with(
                        StatusCode::BAD_REQUEST,
                        "apiModels.uploadMissingFileName",
                        vec![("index", (i + 1).to_string())],
                    ));
                }
            }
        } else {
            paths[i].clone()
        };

        let rel = sanitize_relative_path(&raw).ok_or_else(|| {
            err_with(
                StatusCode::BAD_REQUEST,
                "apiModels.uploadPathInvalid",
                vec![("path", raw.clone())],
            )
        })?;
        if !seen.insert(rel.clone()) {
            return Err(err_with(
                StatusCode::BAD_REQUEST,
                "apiModels.uploadPathDuplicate",
                vec![("path", raw.clone())],
            ));
        }
        let dest = resolve_within(staging, &rel).ok_or_else(|| {
            err_with(
                StatusCode::BAD_REQUEST,
                "apiModels.uploadPathInvalid",
                vec![("path", raw.clone())],
            )
        })?;

        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                err_detail(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "apiModels.uploadMkdirFailed",
                    e,
                )
            })?;
        }
        if let Err(e) = tokio::fs::rename(&sf.temp, &dest).await {
            // 同目录 rename 正常不会失败；兜底复制（parts_dir 与 staging 同根）
            debug!(error = %e, "staging rename failed, falling back to copy");
            tokio::fs::copy(&sf.temp, &dest).await.map_err(|e| {
                err_detail(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "apiModels.uploadPlaceFailed",
                    e,
                )
            })?;
            let _ = tokio::fs::remove_file(&sf.temp).await;
        }
    }
    Ok(())
}

// ─── 落位 ───────────────────────────────────────────────────────────────────

enum FinalizeError {
    /// 目标目录已存在且非空（或被同名文件占用）
    Conflict,
    /// 暂存内容为空（无文件 / 总大小为 0）
    Empty,
    /// 其他 I/O 失败：(i18n 键, 错误细节)
    Other(&'static str, String),
}

/// 把模型根移动到 models/<target_dir>（阻塞任务）：
/// 优先 rename（staging 与 models 同根，通常同一文件系统）；
/// rename 失败（如跨设备 EXDEV）时回退为递归复制 + 删除源目录。
/// 返回落位后的目录总大小。
fn finalize_blocking(model_root: &Path, target: &Path) -> Result<u64, FinalizeError> {
    // 校验暂存非空、总大小 > 0
    let total_before = dir_total_size(model_root);
    let has_entries = std::fs::read_dir(model_root)
        .map(|mut e| e.next().is_some())
        .unwrap_or(false);
    if !has_entries || total_before == 0 {
        return Err(FinalizeError::Empty);
    }

    // 双重检查目标（handler 入口已尽早检查一次，这里防 TOCTOU）
    if target.exists() {
        let empty_dir = target.is_dir()
            && std::fs::read_dir(target)
                .map(|mut e| e.next().is_none())
                .unwrap_or(false);
        if !empty_dir {
            return Err(FinalizeError::Conflict);
        }
        // 空目录：删除以便 rename
        let _ = std::fs::remove_dir(target);
    }
    if let Some(parent) = target.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    match std::fs::rename(model_root, target) {
        Ok(()) => Ok(dir_total_size(target)),
        Err(rename_err) => {
            debug!(
                error = %rename_err,
                "rename failed (likely cross-device); falling back to recursive copy"
            );
            // 回退复制前再次确认目标不存在，绝不把文件合并进别人已有的目录
            if target.exists() {
                return Err(FinalizeError::Conflict);
            }
            if let Err(e) = std::fs::create_dir_all(target) {
                return Err(FinalizeError::Other(
                    "apiModels.targetDirCreateFailed",
                    e.to_string(),
                ));
            }
            if let Err(e) = copy_dir_contents(model_root, target) {
                // target 是刚创建、只含本次复制产物的目录，失败时安全删除
                let _ = std::fs::remove_dir_all(target);
                return Err(FinalizeError::Other(
                    "apiModels.uploadCopyFailed",
                    e.to_string(),
                ));
            }
            let _ = std::fs::remove_dir_all(model_root);
            Ok(dir_total_size(target))
        }
    }
}

/// finalize 的 async 包装（阻塞部分放入 spawn_blocking）
async fn finalize_staging(model_root: PathBuf, target: PathBuf) -> Result<u64, FinalizeError> {
    tokio::task::spawn_blocking(move || finalize_blocking(&model_root, &target))
        .await
        .map_err(|e| {
            FinalizeError::Other("apiModels.uploadFinalizePanicked", e.to_string())
        })?
}

/// 递归复制 src 目录内容到 dst（同步，供 spawn_blocking 调用）
fn copy_dir_contents(src: &Path, dst: &Path) -> std::io::Result<()> {
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let ft = entry.file_type()?;
        if ft.is_dir() {
            std::fs::create_dir_all(&to)?;
            copy_dir_contents(&from, &to)?;
        } else if ft.is_file() {
            std::fs::copy(&from, &to)?;
        }
        // 符号链接等其他类型：跳过
    }
    Ok(())
}

/// 目标路径是否已被占用（非空目录或同名文件存在）
async fn target_blocked(target: &Path) -> bool {
    if target.is_file() {
        return true;
    }
    if !target.is_dir() {
        return false;
    }
    match tokio::fs::read_dir(target).await {
        Ok(mut entries) => matches!(entries.next_entry().await, Ok(Some(_))),
        // 读不了目录：保守视为未占用，落位阶段会再次检查
        Err(_) => false,
    }
}

// ─── 路径安全 ───────────────────────────────────────────────────────────────

/// 清洗浏览器提供的相对路径（文件夹模式 paths / file_name）。
///
/// 拒绝：空路径、绝对路径（POSIX `/` 与 Windows `C:`、`\` 前缀）、
/// 任何 `..` 分段。`.` 分段与冗余分隔符会被归一化去掉。
/// 合法时返回清洗后的相对路径。
fn sanitize_relative_path(raw: &str) -> Option<PathBuf> {
    if raw.is_empty() {
        return None;
    }
    let bytes = raw.as_bytes();
    // POSIX 绝对路径，或 UNC / 反斜杠开头的 Windows 路径
    if bytes[0] == b'/' || bytes[0] == b'\\' {
        return None;
    }
    // Windows 盘符前缀（"C:..."）
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return None;
    }

    let mut out = PathBuf::new();
    for seg in raw.split(['/', '\\']) {
        match seg {
            "" | "." => {}       // 冗余分隔符 / 当前目录分段：忽略
            ".." => return None, // 目录穿越：直接拒绝
            s => out.push(s),
        }
    }
    if out.as_os_str().is_empty() {
        None
    } else {
        Some(out)
    }
}

/// 把相对路径拼接到 base 下，并保证结果不越出 base（纵深防御）。
///
/// `starts_with` 是词法前缀比较，不会归一化 `..`，因此先显式拒绝
/// 绝对路径与含 `..` 分段的 rel，再做前缀校验。
fn resolve_within(base: &Path, rel: &Path) -> Option<PathBuf> {
    if rel.is_absolute()
        || rel
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return None;
    }
    let joined = base.join(rel);
    if joined.starts_with(base) {
        Some(joined)
    } else {
        None
    }
}

// ─── 归档解包 ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
enum ArchiveKind {
    Zip,
    TarGz,
}

/// 按文件名判断归档类型（大小写不敏感）
fn classify_archive(name: &str) -> Option<ArchiveKind> {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".zip") {
        Some(ArchiveKind::Zip)
    } else if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") {
        Some(ArchiveKind::TarGz)
    } else {
        None
    }
}

/// 解压 zip 到 dest（同步，供 spawn_blocking 调用）。
///
/// zip-slip 防御：先用 zip crate 的 `enclosed_name()`（拒绝绝对路径/越界），
/// 再走 `sanitize_relative_path` 做二次分段检查，任一失败即整体报错。
fn extract_zip(archive_path: &Path, dest: &Path) -> Result<(), ExtractError> {
    let file = std::fs::File::open(archive_path)
        .map_err(|e| ExtractError::detail("apiModels.archiveOpenFailed", e))?;
    let mut archive = zip::ZipArchive::new(std::io::BufReader::new(file))
        .map_err(|e| ExtractError::detail("apiModels.archiveParseFailed", e))?;

    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| ExtractError::detail("apiModels.archiveEntryReadFailed", e))?;

        let rel = entry
            .enclosed_name()
            .and_then(|p| sanitize_relative_path(p.to_str()?))
            .ok_or_else(|| {
                ExtractError::entry("apiModels.archiveUnsafePath", entry.name())
            })?;

        let out = dest.join(&rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&out)
                .map_err(|e| ExtractError::detail("apiModels.uploadMkdirFailed", e))?;
        } else {
            if let Some(parent) = out.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| ExtractError::detail("apiModels.uploadMkdirFailed", e))?;
            }
            let mut f = std::fs::File::create(&out)
                .map_err(|e| ExtractError::detail("apiModels.uploadCreateFileFailed", e))?;
            // io::copy 内部按缓冲区分块读写，不会整文件进内存
            std::io::copy(&mut entry, &mut f)
                .map_err(|e| ExtractError::detail("apiModels.archiveExtractEntryFailed", e))?;
        }
    }
    Ok(())
}

/// 解压 .tar.gz / .tgz 到 dest（同步，供 spawn_blocking 调用）。
///
/// zip-slip 防御：逐条目用 `sanitize_relative_path` 校验 entry 路径，
/// 符号链接 / 硬链接的目标同样校验，防止链接指向解包目录之外。
fn extract_tar_gz(archive_path: &Path, dest: &Path) -> Result<(), ExtractError> {
    let file = std::fs::File::open(archive_path)
        .map_err(|e| ExtractError::detail("apiModels.archiveOpenFailed", e))?;
    let decoder = flate2::read::GzDecoder::new(std::io::BufReader::new(file));
    let mut archive = tar::Archive::new(decoder);

    let entries = archive
        .entries()
        .map_err(|e| ExtractError::detail("apiModels.archiveParseFailed", e))?;
    for entry in entries {
        let mut entry =
            entry.map_err(|e| ExtractError::detail("apiModels.archiveEntryReadFailed", e))?;

        let raw = entry
            .path()
            .map_err(|e| ExtractError::detail("apiModels.archiveEntryPathInvalid", e))?
            .to_string_lossy()
            .into_owned();
        let rel = sanitize_relative_path(&raw)
            .ok_or_else(|| ExtractError::entry("apiModels.archiveUnsafePath", &raw))?;

        // 链接目标也必须安全，防止通过 symlink 逃逸（读取失败时跳过，
        // 后续 unpack 会自行报错）
        if let Ok(Some(link)) = entry.link_name() {
            let link_raw = link.to_string_lossy();
            if sanitize_relative_path(&link_raw).is_none() {
                return Err(ExtractError::entry(
                    "apiModels.archiveUnsafeLink",
                    &link_raw,
                ));
            }
        }

        let out = dest.join(&rel);
        // unpack 不会创建缺失的父目录（tar 内未必有显式目录条目），先补齐
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| ExtractError::detail("apiModels.uploadMkdirFailed", e))?;
        }
        // unpack 写入调用方给定的（已清洗）路径；目录条目会创建对应目录
        entry
            .unpack(&out)
            .map_err(|e| ExtractError::detail("apiModels.archiveExtractEntryFailed", e))?;
    }
    Ok(())
}

/// 若解包目录恰好只有一个顶层目录（无其他条目），返回该目录（剥一层）
async fn single_top_dir(extract_dir: &Path) -> Option<PathBuf> {
    let mut entries = tokio::fs::read_dir(extract_dir).await.ok()?;
    let mut found: Option<PathBuf> = None;
    while let Ok(Some(entry)) = entries.next_entry().await {
        if found.is_some() {
            return None; // 多于一个顶层条目
        }
        found = Some(entry.path());
    }
    match found {
        Some(p) if p.is_dir() => Some(p),
        _ => None,
    }
}

// ─── 辅助 ───────────────────────────────────────────────────────────────────

/// 生成暂存目录名（不引入 uuid crate：纳秒时间戳 + 进程内序号 + pid）
fn staging_id() -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{nanos:x}-{seq:04x}-{}", std::process::id())
}

/// 从 AppState 构建 ModelManager（附带全部 manifest，与现有 handler 保持一致）
async fn build_model_manager(state: &AppState) -> ModelManager {
    let config = state.config.read().await;
    let modules = state.modules.read().await;
    let manifests = modules.iter().filter_map(|m| m.manifest.clone()).collect();
    ModelManager::new(&config.models, &state.root).with_manifests(manifests)
}

/// 查找模块的 manifest（通过 module_id）
async fn find_module_manifest(state: &AppState, module_id: &str) -> Option<ModuleManifest> {
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

// ─── 直跑输入上传（§8.1 POST /api/upload/input，Wave 2 B4 新增段） ──────────

/// Windows 保留设备名（作为文件名主干时非法，清洗时加前缀规避）
const WINDOWS_RESERVED_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// 清洗浏览器提供的上传文件名（双平台非法字符一律替换/规避）。
///
/// 规则：
/// - 仅取最后一段（剥离浏览器可能携带的目录前缀，防路径注入）；
/// - 非法字符 `<>:"/\|?*` 与控制字符替换为 `_`；
/// - Windows 保留设备名（CON/NUL/COM1…，大小写不敏感、含扩展名形态）前缀 `_`；
/// - 去除 Windows 不允许的结尾 `.` 与空格；
/// - 主干过长截断（保留扩展名，避开 255 字节文件系统上限）；
/// - 清洗后为空 → 生成兜底名。
///
/// 永不失败：任何输入都产出一个双平台可落盘的文件名。
fn sanitize_file_name(raw: &str) -> String {
    // 1. 剥离目录前缀（兼容 `/` 与 `\` 两种分隔）
    let base = raw.rsplit(['/', '\\']).next().unwrap_or("");

    // 2. 非法字符 → '_'（含控制字符；保留 Unicode 字母/数字/CJK）
    let mut cleaned: String = base
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') {
                '_'
            } else {
                c
            }
        })
        .collect();

    // 3. 去除 Windows 不允许的结尾 '.' 与空格
    let trimmed = cleaned.trim_end_matches(['.', ' ']);
    cleaned = trimmed.to_string();

    // 4. Windows 保留设备名：主干命中即前缀 '_'（CON.txt 同样非法）
    let stem = cleaned.split('.').next().unwrap_or("");
    if WINDOWS_RESERVED_NAMES
        .iter()
        .any(|r| r.eq_ignore_ascii_case(stem))
    {
        cleaned = format!("_{cleaned}");
    }

    // 5. 主干截断（保留扩展名）
    if cleaned.len() > 240 {
        let (stem, ext) = match cleaned.rsplit_once('.') {
            Some((s, e)) if !e.is_empty() && e.len() <= 20 => (s.to_string(), Some(e.to_string())),
            _ => (cleaned.clone(), None),
        };
        let truncated: String = stem.chars().take(200).collect();
        cleaned = match ext {
            Some(e) => format!("{truncated}.{e}"),
            None => truncated,
        };
    }

    // 6. 兜底
    if cleaned.is_empty() {
        format!("input-{}", staging_id())
    } else {
        cleaned
    }
}

/// POST /api/upload/input — 直跑输入文件上传（§8.1 / §5.3）
///
/// multipart/form-data 单文件，字段名 **`file`**（仲裁 #3 统一）。
/// 流程：流式接收 → tempdir 暂存（不整块进内存）→ 文件名清洗 →
/// workspace/uploads/ 落盘（重名加序号 `-1`/`-2`…，create_new 竞态安全）→
/// 200 `{"path": "<服务器本地绝对路径>"}`（对齐 S2 UploadInputResponse）。
///
/// 返回的路径可直接作为 `POST /api/execute/single` 的 `input_path`。
async fn upload_input(
    State(state): State<Arc<AppState>>,
    multipart: Result<Multipart, MultipartRejection>,
) -> (StatusCode, Json<Value>) {
    let mut multipart = match multipart {
        Ok(m) => m,
        Err(e) => {
            warn!(error = %e, "input upload: multipart rejected");
            return err_response(
                &state,
                StatusCode::BAD_REQUEST,
                "apiModels.uploadMultipartInvalid",
                &[],
            )
            .await;
        }
    };

    // tempdir 暂存：与 workspace 可能不同盘，落盘用 rename + copy 回退
    let temp_dir = std::env::temp_dir().join(format!("ep-input-{}", staging_id()));
    if let Err(e) = tokio::fs::create_dir_all(&temp_dir).await {
        return err_response(
            &state,
            StatusCode::INTERNAL_SERVER_ERROR,
            "apiModels.uploadStagingFailed",
            &[("detail", e.to_string())],
        )
        .await;
    }

    let result = receive_input_file(&mut multipart, &temp_dir).await;

    let staged = match result {
        Ok(Some(staged)) => staged,
        Ok(None) => {
            cleanup_temp_dir(&temp_dir).await;
            return err_response(
                &state,
                StatusCode::BAD_REQUEST,
                "apiModels.inputUploadMissingFile",
                &[],
            )
            .await;
        }
        Err(ue) => {
            cleanup_temp_dir(&temp_dir).await;
            return err_response(&state, ue.status, ue.key, &ue.params).await;
        }
    };
    let (temp_path, raw_name) = staged;

    // 落盘到 workspace/uploads/
    let uploads_dir = {
        let cfg = state.config.read().await;
        cfg.resolve_workspace_dir(&state.root).join("uploads")
    };
    if let Err(e) = tokio::fs::create_dir_all(&uploads_dir).await {
        cleanup_temp_dir(&temp_dir).await;
        return err_response(
            &state,
            StatusCode::INTERNAL_SERVER_ERROR,
            "apiModels.uploadStagingFailed",
            &[("detail", e.to_string())],
        )
        .await;
    }

    let placed = place_input_file(&temp_path, &uploads_dir, &raw_name).await;

    // 无论成败清理 tempdir（落盘成功后其中只剩空壳）
    cleanup_temp_dir(&temp_dir).await;

    match placed {
        Ok(final_path) => {
            info!(path = %final_path.display(), "API: input upload completed");
            (
                StatusCode::OK,
                Json(json!({ "path": final_path.display().to_string() })),
            )
        }
        Err(ue) => err_response(&state, ue.status, ue.key, &ue.params).await,
    }
}

/// 清理 tempdir（NotFound 视为成功，其余仅告警）
async fn cleanup_temp_dir(dir: &Path) {
    if let Err(e) = tokio::fs::remove_dir_all(dir).await {
        if e.kind() != std::io::ErrorKind::NotFound {
            warn!(dir = %dir.display(), error = %e, "input upload: tempdir cleanup failed");
        }
    }
}

/// 从 multipart 流式接收首个 `file` 字段到 tempdir，返回 (暂存路径, 原始文件名)。
/// 无 file 字段 → Ok(None)；其余字段忽略。
async fn receive_input_file(
    multipart: &mut Multipart,
    temp_dir: &Path,
) -> Result<Option<(PathBuf, String)>, UploadError> {
    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|e| err_detail(StatusCode::BAD_REQUEST, "apiModels.uploadReadFailed", e))?
    {
        let is_file = field.name() == Some("file");
        if !is_file {
            continue; // 未知字段跳过（multer 自动丢弃未读内容）
        }

        let raw_name = field.file_name().unwrap_or("").to_string();
        let temp_path = temp_dir.join("upload.part");
        let mut file = tokio::fs::File::create(&temp_path)
            .await
            .map_err(|e| {
                err_detail(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "apiModels.uploadStagingFileFailed",
                    e,
                )
            })?;
        // 逐 chunk 流式写盘，绝不整块读入内存
        while let Some(chunk) = field
            .chunk()
            .await
            .map_err(|e| err_detail(StatusCode::BAD_REQUEST, "apiModels.uploadReadFailed", e))?
        {
            file.write_all(&chunk).await.map_err(|e| {
                err_detail(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "apiModels.uploadWriteFailed",
                    e,
                )
            })?;
        }
        file.flush()
            .await
            .map_err(|e| err_detail(StatusCode::INTERNAL_SERVER_ERROR, "apiModels.uploadWriteFailed", e))?;
        return Ok(Some((temp_path, raw_name)));
    }
    Ok(None)
}

/// 把暂存文件落位到 uploads_dir：清洗文件名 + 冲突加序号。
///
/// 竞态安全：`create_new` 原子占位抢占文件名；POSIX 上 rename 直接原子替换
/// 占位文件；Windows 上 rename 不覆盖已存在文件，先删占位再 rename——
/// 删除与 rename 之间被并发抢占时 rename 失败，换序号重试，绝不覆盖他人文件。
async fn place_input_file(
    temp_path: &Path,
    uploads_dir: &Path,
    raw_name: &str,
) -> Result<PathBuf, UploadError> {
    let clean = sanitize_file_name(raw_name);
    let (stem, ext) = match clean.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() && !e.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (clean.clone(), String::new()),
    };

    // 冲突命名：name.ext → name-1.ext → name-2.ext …（上限内未命中即报错）
    for seq in 0..1000u32 {
        let candidate = if seq == 0 {
            uploads_dir.join(format!("{stem}{ext}"))
        } else {
            uploads_dir.join(format!("{stem}-{seq}{ext}"))
        };
        match tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
            .await
        {
            Ok(reserved) => {
                drop(reserved); // 释放句柄后处置 0 字节占位文件
                if cfg!(target_os = "windows") {
                    // Windows：rename 拒绝覆盖已存在文件 → 删占位后 rename 抢占
                    let _ = tokio::fs::remove_file(&candidate).await;
                    match tokio::fs::rename(temp_path, &candidate).await {
                        Ok(()) => return Ok(candidate),
                        Err(_) => {
                            if tokio::fs::try_exists(&candidate).await.unwrap_or(false) {
                                continue; // 并发抢占 → 换序号
                            }
                            return copy_into_place(temp_path, &candidate).await; // 跨盘等
                        }
                    }
                }
                // POSIX：rename 原子替换占位文件（同盘）；跨盘回退 copy
                return move_into_place(temp_path, &candidate).await;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => {
                return Err(err_detail(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "apiModels.inputUploadPlaceFailed",
                    e,
                ));
            }
        }
    }
    Err(err_detail(
        StatusCode::INTERNAL_SERVER_ERROR,
        "apiModels.inputUploadPlaceFailed",
        "too many file name collisions (>1000)",
    ))
}

/// tempdir → uploads 落位（POSIX 路径）：rename 替换占位文件；跨盘回退 copy
async fn move_into_place(temp_path: &Path, dest: &Path) -> Result<PathBuf, UploadError> {
    if tokio::fs::rename(temp_path, dest).await.is_ok() {
        return Ok(dest.to_path_buf());
    }
    copy_into_place(temp_path, dest).await
}

/// 跨盘回退：copy + 删源（调用方已保证 dest 不存在或为本次占位）
async fn copy_into_place(temp_path: &Path, dest: &Path) -> Result<PathBuf, UploadError> {
    tokio::fs::copy(temp_path, dest).await.map_err(|e| {
        err_detail(
            StatusCode::INTERNAL_SERVER_ERROR,
            "apiModels.inputUploadPlaceFailed",
            e,
        )
    })?;
    let _ = tokio::fs::remove_file(temp_path).await;
    Ok(dest.to_path_buf())
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use ep_core::config::AppConfig;
    use ep_core::module::discovery::{DiscoveredModule, DiscoveryStatus};
    use ep_core::port::PortManager;

    static TEST_SEQ: AtomicUsize = AtomicUsize::new(0);

    const BOUNDARY: &str = "----ep-test-boundary";

    const TEST_MANIFEST_TOML: &str = r#"
[module]
id = "test-module"
name = "测试模块"
version = "0.1.0"
description = "upload test module"
category = "asr"
genre = "test"

[runtime]
type = "python"
python_version = ">=3.10"

[compute]
backends = ["cpu"]

[[models]]
id = "m1"
name = "测试模型"
source = "url"
url = "auto"
target_dir = "test-model"
default = true

[[models]]
id = "m-hf"
name = "HF 模型"
source = "huggingface"
repo_id = "org/repo"
target_dir = "hf-model"

[interface]
type = "http"
"#;

    fn unique_root(tag: &str) -> PathBuf {
        let seq = TEST_SEQ.fetch_add(1, Ordering::SeqCst);
        let root =
            std::env::temp_dir().join(format!("ep-upload-{tag}-{}-{seq}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn test_state(root: PathBuf) -> Arc<AppState> {
        test_state_with_language(root, "zh-CN")
    }

    /// 同 test_state，可指定 UI 语言（config.general.language）
    fn test_state_with_language(root: PathBuf, language: &str) -> Arc<AppState> {
        let manifest: ModuleManifest = toml::from_str(TEST_MANIFEST_TOML).unwrap();
        let module = DiscoveredModule {
            manifest: Some(manifest),
            path: root.join("modules").join("test-module"),
            status: DiscoveryStatus::Valid,
        };
        let mut config = AppConfig::default();
        config.general.language = language.to_string();
        Arc::new(AppState::new(
            root,
            config,
            vec![],
            vec![module],
            PortManager::new(18000, 19000),
        ))
    }

    /// 向 multipart body 追加一个 part
    fn form_part(buf: &mut Vec<u8>, name: &str, filename: Option<&str>, data: &[u8]) {
        buf.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
        match filename {
            Some(f) => buf.extend_from_slice(
                format!(
                    "Content-Disposition: form-data; name=\"{name}\"; filename=\"{f}\"\r\n\
                     Content-Type: application/octet-stream\r\n\r\n"
                )
                .as_bytes(),
            ),
            None => buf.extend_from_slice(
                format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes(),
            ),
        }
        buf.extend_from_slice(data);
        buf.extend_from_slice(b"\r\n");
    }

    fn finish_multipart(buf: &mut Vec<u8>) {
        buf.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
    }

    fn upload_request(uri: &str, body: Vec<u8>) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header(
                "content-type",
                format!("multipart/form-data; boundary={BOUNDARY}"),
            )
            .header("content-length", body.len().to_string())
            .body(Body::from(body))
            .unwrap()
    }

    async fn response_json(resp: axum::response::Response) -> (StatusCode, Value) {
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: Value = serde_json::from_slice(&bytes)
            .unwrap_or_else(|e| panic!("响应不是合法 JSON: {e}; body={bytes:?}"));
        (status, json)
    }

    /// 用 zip crate 在内存中构造 zip（可包含恶意路径，用于 zip-slip 测试）
    fn build_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        for (name, data) in entries {
            writer.start_file(*name, options).unwrap();
            std::io::Write::write_all(&mut writer, data).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    // ─── 路径清洗单测 ────────────────────────────────────────────────────

    #[test]
    fn sanitize_rejects_absolute_and_traversal() {
        // 绝对路径
        assert!(sanitize_relative_path("/etc/passwd").is_none());
        assert!(sanitize_relative_path("C:\\Windows\\system32").is_none());
        assert!(sanitize_relative_path("C:/models/x.bin").is_none());
        assert!(sanitize_relative_path("\\\\server\\share").is_none());
        // .. 分段
        assert!(sanitize_relative_path("..").is_none());
        assert!(sanitize_relative_path("../evil.bin").is_none());
        assert!(sanitize_relative_path("a/../b.bin").is_none());
        assert!(sanitize_relative_path("a\\..\\b.bin").is_none());
        // 空路径 / 仅 . 分段
        assert!(sanitize_relative_path("").is_none());
        assert!(sanitize_relative_path(".").is_none());
        assert!(sanitize_relative_path("././").is_none());
    }

    #[test]
    fn sanitize_accepts_normal_relative_paths() {
        assert_eq!(
            sanitize_relative_path("sub/dir/file.bin").unwrap(),
            PathBuf::from("sub/dir/file.bin")
        );
        assert_eq!(
            sanitize_relative_path("./model.bin").unwrap(),
            PathBuf::from("model.bin")
        );
        assert_eq!(
            sanitize_relative_path("a//b/c.bin").unwrap(),
            PathBuf::from("a/b/c.bin")
        );
        assert_eq!(
            sanitize_relative_path("model.bin").unwrap(),
            PathBuf::from("model.bin")
        );
    }

    #[test]
    fn resolve_within_never_escapes_base() {
        let base = PathBuf::from("/data/models/.upload-staging/x");
        assert!(resolve_within(&base, &PathBuf::from("a/b.bin")).is_some());
        // 即使绕过清洗直接传 ..，也必须被拒绝
        assert!(resolve_within(&base, &PathBuf::from("../../etc")).is_none());
    }

    // ─── zip-slip 守卫单测 ───────────────────────────────────────────────

    #[test]
    fn extract_zip_rejects_zip_slip_entries() {
        let root = unique_root("zipslip");
        let archive = root.join("evil.zip");
        let dest = root.join("extract");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(
            &archive,
            build_zip(&[("../evil.txt", b"pwned"), ("ok.txt", b"fine")]),
        )
        .unwrap();

        let result = extract_zip(&archive, &dest);
        assert!(result.is_err(), "必须拒绝含 .. 的条目");
        assert_eq!(result.unwrap_err().key, "apiModels.archiveUnsafePath");
        // .. 条目绝不能落到解包目录之外
        assert!(!root.join("evil.txt").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn extract_zip_rejects_nested_traversal() {
        let root = unique_root("zipslip-nested");
        let archive = root.join("nested.zip");
        let dest = root.join("extract");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(&archive, build_zip(&[("a/../../evil2.txt", b"pwned")])).unwrap();

        assert!(extract_zip(&archive, &dest).is_err());
        assert!(!root.join("evil2.txt").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn extract_zip_ok_for_safe_archive() {
        let root = unique_root("zip-ok");
        let archive = root.join("good.zip");
        let dest = root.join("extract");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(
            &archive,
            build_zip(&[("sub/model.bin", b"weights"), ("sub/dir/cfg.json", b"{}")]),
        )
        .unwrap();

        extract_zip(&archive, &dest).unwrap();
        assert_eq!(
            std::fs::read(dest.join("sub/model.bin")).unwrap(),
            b"weights"
        );
        assert!(dest.join("sub/dir/cfg.json").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 用 tar + flate2 在内存中构造 .tar.gz
    fn build_tar_gz(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (name, data) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, name, *data).unwrap();
        }
        let tar_bytes = builder.into_inner().unwrap();
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut encoder, &tar_bytes).unwrap();
        encoder.finish().unwrap()
    }

    #[test]
    fn extract_tar_gz_ok_for_safe_archive() {
        let root = unique_root("targz-ok");
        let archive = root.join("good.tar.gz");
        let dest = root.join("extract");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(
            &archive,
            build_tar_gz(&[("sub/w.bin", b"tar-weights"), ("readme.txt", b"hi")]),
        )
        .unwrap();

        extract_tar_gz(&archive, &dest).unwrap();
        assert_eq!(
            std::fs::read(dest.join("sub/w.bin")).unwrap(),
            b"tar-weights"
        );
        assert_eq!(std::fs::read(dest.join("readme.txt")).unwrap(), b"hi");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn extract_tar_gz_rejects_traversal_entries() {
        let root = unique_root("targz-slip");
        let archive = root.join("evil.tar.gz");
        let dest = root.join("extract");
        std::fs::create_dir_all(&dest).unwrap();

        // tar::Builder 自身会拒绝含 .. 的路径，这里直接构造裸 header 绕过
        let mut builder = tar::Builder::new(Vec::new());
        let data = b"pwned";
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        // 手工写入含 .. 的路径字节，模拟恶意构造的归档
        {
            let path = b"../evil-tar.txt";
            let header_bytes = header.as_mut_bytes();
            // GNU tar name 字段：0..100
            header_bytes[..path.len()].copy_from_slice(path);
        }
        header.set_cksum();
        builder
            .append(&header, &data[..])
            .expect("raw header append");
        let tar_bytes = builder.into_inner().unwrap();
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut encoder, &tar_bytes).unwrap();
        std::fs::write(&archive, encoder.finish().unwrap()).unwrap();

        let result = extract_tar_gz(&archive, &dest);
        assert!(result.is_err(), "必须拒绝含 .. 的条目");
        assert!(!root.join("evil-tar.txt").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    // ─── handler 集成测试 ────────────────────────────────────────────────

    #[tokio::test]
    async fn upload_success_roundtrip() {
        let root = unique_root("ok");
        let state = test_state(root.clone());
        let app = router().with_state(state);

        let mut body = Vec::new();
        form_part(&mut body, "model_id", None, b"m1");
        form_part(&mut body, "files", Some("a.bin"), b"hello-model");
        form_part(&mut body, "files", Some("b.bin"), b"nested-data");
        form_part(&mut body, "paths", None, b"a.bin");
        form_part(&mut body, "paths", None, b"sub/dir/b.bin");
        finish_multipart(&mut body);

        let resp = app
            .oneshot(upload_request("/models/test-module/upload", body))
            .await
            .unwrap();
        let (status, json) = response_json(resp).await;

        assert_eq!(status, StatusCode::OK, "响应: {json}");
        assert_eq!(json["model_id"], "m1");
        assert_eq!(json["status"], "ready");
        assert_eq!(json["target_dir"], "test-model");
        assert!(json["size_bytes"].as_u64().unwrap() > 0);

        // 文件内容回环校验
        let model_dir = root.join("models/test-model");
        assert_eq!(
            std::fs::read(model_dir.join("a.bin")).unwrap(),
            b"hello-model"
        );
        assert_eq!(
            std::fs::read(model_dir.join("sub/dir/b.bin")).unwrap(),
            b"nested-data"
        );

        // .ep_meta.json：上传按本地导入记录
        let meta: Value = serde_json::from_str(
            &std::fs::read_to_string(model_dir.join(".ep_meta.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(meta["source"], "local_import");
        assert_eq!(meta["model_id"], "m1");
        assert_eq!(meta["module_id"], "test-module");

        // staging 已清理
        assert!(!root.join("models/.upload-staging").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn upload_conflict_returns_409() {
        let root = unique_root("conflict");
        // 预置已存在的模型目录
        let model_dir = root.join("models/test-model");
        std::fs::create_dir_all(&model_dir).unwrap();
        std::fs::write(model_dir.join("keep.bin"), b"existing").unwrap();

        let state = test_state(root.clone());
        let app = router().with_state(state);

        let mut body = Vec::new();
        form_part(&mut body, "model_id", None, b"m1");
        form_part(&mut body, "files", Some("a.bin"), b"new-data");
        form_part(&mut body, "paths", None, b"a.bin");
        finish_multipart(&mut body);

        let resp = app
            .oneshot(upload_request("/models/test-module/upload", body))
            .await
            .unwrap();
        let (status, json) = response_json(resp).await;

        assert_eq!(status, StatusCode::CONFLICT);
        assert!(json["error"].as_str().unwrap().contains("模型已存在"));
        // 原有文件未被破坏，staging 已清理
        assert_eq!(
            std::fs::read(model_dir.join("keep.bin")).unwrap(),
            b"existing"
        );
        assert!(!model_dir.join("a.bin").exists());
        assert!(!root.join("models/.upload-staging").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn upload_unknown_model_returns_404() {
        let root = unique_root("no-model");
        let state = test_state(root.clone());
        let app = router().with_state(state);

        let mut body = Vec::new();
        form_part(&mut body, "model_id", None, b"ghost");
        form_part(&mut body, "files", Some("a.bin"), b"x");
        finish_multipart(&mut body);

        let resp = app
            .oneshot(upload_request("/models/test-module/upload", body))
            .await
            .unwrap();
        let (status, json) = response_json(resp).await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(json["error"].as_str().unwrap().contains("模型声明不存在"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn upload_unknown_module_returns_404() {
        let root = unique_root("no-module");
        let state = test_state(root.clone());
        let app = router().with_state(state);

        let mut body = Vec::new();
        form_part(&mut body, "model_id", None, b"m1");
        form_part(&mut body, "files", Some("a.bin"), b"x");
        finish_multipart(&mut body);

        let resp = app
            .oneshot(upload_request("/models/no-such-module/upload", body))
            .await
            .unwrap();
        let (status, json) = response_json(resp).await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        // 与下载/导入共用 moduleNotFound 键（带模块 ID 插值）
        assert_eq!(json["error"], "模块 'no-such-module' 不存在");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn upload_paths_mismatch_returns_400() {
        let root = unique_root("mismatch");
        let state = test_state(root.clone());
        let app = router().with_state(state);

        let mut body = Vec::new();
        form_part(&mut body, "model_id", None, b"m1");
        form_part(&mut body, "files", Some("a.bin"), b"x");
        form_part(&mut body, "paths", None, b"a.bin");
        form_part(&mut body, "paths", None, b"b.bin"); // 多出一个 path
        finish_multipart(&mut body);

        let resp = app
            .oneshot(upload_request("/models/test-module/upload", body))
            .await
            .unwrap();
        let (status, json) = response_json(resp).await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(json["error"].as_str().unwrap().contains("不一致"));
        assert!(!root.join("models/test-model").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn upload_traversal_path_returns_400() {
        let root = unique_root("traversal");
        let state = test_state(root.clone());
        let app = router().with_state(state);

        let mut body = Vec::new();
        form_part(&mut body, "model_id", None, b"m1");
        form_part(&mut body, "files", Some("evil.bin"), b"pwned");
        form_part(&mut body, "paths", None, b"../../evil.bin");
        finish_multipart(&mut body);

        let resp = app
            .oneshot(upload_request("/models/test-module/upload", body))
            .await
            .unwrap();
        let (status, json) = response_json(resp).await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(json["error"].as_str().unwrap().contains("非法文件路径"));
        assert!(!root.join("evil.bin").exists());
        assert!(!root.join("models/evil.bin").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// language=en 时上传错误返回英文文案（路径穿越 400，警示语义保持）
    #[tokio::test]
    async fn upload_error_in_english_when_language_en() {
        let root = unique_root("lang-en");
        let state = test_state_with_language(root.clone(), "en");
        let app = router().with_state(state);

        let mut body = Vec::new();
        form_part(&mut body, "model_id", None, b"m1");
        form_part(&mut body, "files", Some("evil.bin"), b"pwned");
        form_part(&mut body, "paths", None, b"../../evil.bin");
        finish_multipart(&mut body);

        let resp = app
            .oneshot(upload_request("/models/test-module/upload", body))
            .await
            .unwrap();
        let (status, json) = response_json(resp).await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        let err = json["error"].as_str().unwrap();
        assert!(
            err.starts_with("Invalid file path"),
            "expected English error, got: {err}"
        );
        assert!(!root.join("evil.bin").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn upload_archive_zip_strips_single_top_dir() {
        let root = unique_root("archive-strip");
        let state = test_state(root.clone());
        let app = router().with_state(state);

        // 压缩包内只有一个顶层目录 myroot → 应剥掉一层
        let zip_bytes = build_zip(&[
            ("myroot/weights.bin", b"archive-weights"),
            ("myroot/config.json", b"{}"),
        ]);

        let mut body = Vec::new();
        form_part(&mut body, "model_id", None, b"m1");
        form_part(&mut body, "files", Some("model.zip"), &zip_bytes);
        finish_multipart(&mut body);

        let resp = app
            .oneshot(upload_request("/models/test-module/upload", body))
            .await
            .unwrap();
        let (status, json) = response_json(resp).await;

        assert_eq!(status, StatusCode::OK, "响应: {json}");
        let model_dir = root.join("models/test-model");
        assert_eq!(
            std::fs::read(model_dir.join("weights.bin")).unwrap(),
            b"archive-weights"
        );
        assert!(!model_dir.join("myroot").exists(), "顶层目录应被剥掉");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn upload_archive_zip_keeps_multiple_top_entries() {
        let root = unique_root("archive-flat");
        let state = test_state(root.clone());
        let app = router().with_state(state);

        // 多个顶层条目 → 不剥层，整个内容即模型根
        let zip_bytes = build_zip(&[("a.bin", b"aa"), ("b.bin", b"bb")]);

        let mut body = Vec::new();
        form_part(&mut body, "model_id", None, b"m1");
        form_part(&mut body, "files", Some("model.zip"), &zip_bytes);
        form_part(&mut body, "paths", None, b"model.zip"); // paths 仅一项也触发归档模式
        finish_multipart(&mut body);

        let resp = app
            .oneshot(upload_request("/models/test-module/upload", body))
            .await
            .unwrap();
        let (status, json) = response_json(resp).await;

        assert_eq!(status, StatusCode::OK, "响应: {json}");
        let model_dir = root.join("models/test-model");
        assert_eq!(std::fs::read(model_dir.join("a.bin")).unwrap(), b"aa");
        assert_eq!(std::fs::read(model_dir.join("b.bin")).unwrap(), b"bb");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn upload_archive_tgz_roundtrip() {
        let root = unique_root("archive-tgz");
        let state = test_state(root.clone());
        let app = router().with_state(state);

        // .tar.gz 单顶层目录 → 归档模式 + 剥一层
        let tgz_bytes = build_tar_gz(&[("root2/w.bin", b"tgz-weights")]);

        let mut body = Vec::new();
        form_part(&mut body, "model_id", None, b"m1");
        form_part(&mut body, "files", Some("model.tar.gz"), &tgz_bytes);
        finish_multipart(&mut body);

        let resp = app
            .oneshot(upload_request("/models/test-module/upload", body))
            .await
            .unwrap();
        let (status, json) = response_json(resp).await;

        assert_eq!(status, StatusCode::OK, "响应: {json}");
        let model_dir = root.join("models/test-model");
        assert_eq!(
            std::fs::read(model_dir.join("w.bin")).unwrap(),
            b"tgz-weights"
        );
        assert!(!model_dir.join("root2").exists(), "顶层目录应被剥掉");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 超过 axum 默认 2MB body 上限的文件必须能传成功
    ///（证明 DefaultBodyLimit::disable() 对本路由生效）
    #[tokio::test]
    async fn upload_large_file_bypasses_default_body_limit() {
        let root = unique_root("large");
        let state = test_state(root.clone());
        let app = router().with_state(state);

        // 3MB > 默认 2MB 限制
        let payload = vec![0xABu8; 3 * 1024 * 1024];
        let mut body = Vec::new();
        form_part(&mut body, "model_id", None, b"m1");
        form_part(&mut body, "files", Some("big.bin"), &payload);
        form_part(&mut body, "paths", None, b"big.bin");
        finish_multipart(&mut body);

        let resp = app
            .oneshot(upload_request("/models/test-module/upload", body))
            .await
            .unwrap();
        let (status, json) = response_json(resp).await;

        assert_eq!(status, StatusCode::OK, "响应: {json}");
        let saved = std::fs::read(root.join("models/test-model/big.bin")).unwrap();
        assert_eq!(saved.len(), 3 * 1024 * 1024);
        assert!(json["size_bytes"].as_u64().unwrap() >= 3 * 1024 * 1024);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn upload_hf_model_meta_records_repo_id() {
        let root = unique_root("hf-meta");
        let state = test_state(root.clone());
        let app = router().with_state(state);

        let mut body = Vec::new();
        form_part(&mut body, "model_id", None, b"m-hf");
        form_part(&mut body, "files", Some("w.bin"), b"hf-weights");
        form_part(&mut body, "paths", None, b"w.bin");
        finish_multipart(&mut body);

        let resp = app
            .oneshot(upload_request("/models/test-module/upload", body))
            .await
            .unwrap();
        let (status, json) = response_json(resp).await;

        assert_eq!(status, StatusCode::OK, "响应: {json}");
        let meta: Value = serde_json::from_str(
            &std::fs::read_to_string(root.join("models/hf-model/.ep_meta.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(meta["source"], "local_import");
        assert_eq!(meta["repo_id"], "org/repo"); // HF 源填清单 repo_id
        let _ = std::fs::remove_dir_all(&root);
    }

    // ─── 直跑输入上传（/api/upload/input） ───────────────────────────────

    /// 文件名清洗单测：双平台非法字符 / 保留名 / 路径剥离 / 兜底
    #[test]
    fn sanitize_file_name_cleans_invalid_characters() {
        // 非法字符替换
        assert_eq!(sanitize_file_name("a<b>c:d\"e|f?g*.wav"), "a_b_c_d_e_f_g_.wav");
        // 路径剥离（含 Windows 盘符路径）
        assert_eq!(sanitize_file_name("C:\\fake\\dir\\audio.wav"), "audio.wav");
        assert_eq!(sanitize_file_name("/etc/passwd"), "passwd");
        // Windows 保留设备名（含带扩展名形态，大小写不敏感）
        assert_eq!(sanitize_file_name("CON"), "_CON");
        assert_eq!(sanitize_file_name("con.txt"), "_con.txt");
        assert_eq!(sanitize_file_name("Nul.wav"), "_Nul.wav");
        // 结尾点/空格（Windows 不允许）
        assert_eq!(sanitize_file_name("file. "), "file");
        assert_eq!(sanitize_file_name("file..."), "file");
        // 控制字符
        assert_eq!(sanitize_file_name("bad\u{0}name.txt"), "bad_name.txt");
        // 正常名原样
        assert_eq!(sanitize_file_name("语音输入-01.wav"), "语音输入-01.wav");
    }

    #[test]
    fn sanitize_file_name_empty_falls_back() {
        let out = sanitize_file_name("");
        assert!(out.starts_with("input-"), "got: {out}");
        // 仅路径分隔符 → basename 为空 → 兜底
        let out3 = sanitize_file_name("a/b/");
        assert!(out3.starts_with("input-"), "got: {out3}");
        // 全非法字符 → 清洗为下划线（仍是合法文件名，无需兜底）
        assert_eq!(sanitize_file_name("***"), "___");
    }

    #[test]
    fn sanitize_file_name_truncates_overlong_stem() {
        let long_stem = "x".repeat(300);
        let out = sanitize_file_name(&format!("{long_stem}.wav"));
        assert!(out.ends_with(".wav"));
        assert!(out.len() <= 240, "len={}", out.len());

        // 无扩展名同样截断
        let out2 = sanitize_file_name(&"y".repeat(300));
        assert!(out2.len() <= 240, "len={}", out2.len());
    }

    /// 上传往返：文件落盘 workspace/uploads/ + 响应携带服务器本地路径
    #[tokio::test]
    async fn upload_input_roundtrip() {
        let root = unique_root("input-ok");
        let state = test_state(root.clone());
        let app = router().with_state(state);

        let mut body = Vec::new();
        form_part(&mut body, "file", Some("audio.wav"), b"fake-wav-bytes");
        finish_multipart(&mut body);

        let resp = app
            .oneshot(upload_request("/upload/input", body))
            .await
            .unwrap();
        let (status, json) = response_json(resp).await;

        assert_eq!(status, StatusCode::OK, "响应: {json}");
        let path = json["path"].as_str().expect("响应必须带 path").to_string();
        let expected = root.join("workspace").join("uploads").join("audio.wav");
        assert_eq!(PathBuf::from(&path), expected);
        assert_eq!(std::fs::read(&expected).unwrap(), b"fake-wav-bytes");
        // tempdir 无残留（系统临时目录内 ep-input-* 已清理）
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 文件名冲突 → 加序号（-1/-2），绝不覆盖已上传内容
    #[tokio::test]
    async fn upload_input_collision_gets_sequence_suffix() {
        let root = unique_root("input-collide");
        let state = test_state(root.clone());

        for (i, content) in [b"first".as_slice(), b"second".as_slice(), b"third".as_slice()]
            .iter()
            .enumerate()
        {
            let app = router().with_state(state.clone());
            let mut body = Vec::new();
            form_part(&mut body, "file", Some("clip.mp4"), content);
            finish_multipart(&mut body);
            let resp = app
                .oneshot(upload_request("/upload/input", body))
                .await
                .unwrap();
            let (status, json) = response_json(resp).await;
            assert_eq!(status, StatusCode::OK, "第 {} 次上传响应: {json}", i + 1);

            let expected_name = match i {
                0 => "clip.mp4",
                1 => "clip-1.mp4",
                _ => "clip-2.mp4",
            };
            let expected = root.join("workspace").join("uploads").join(expected_name);
            assert_eq!(PathBuf::from(json["path"].as_str().unwrap()), expected);
            assert_eq!(std::fs::read(&expected).unwrap(), *content);
        }

        // 三份内容各自独立，无覆盖
        let uploads = root.join("workspace/uploads");
        assert_eq!(std::fs::read(uploads.join("clip.mp4")).unwrap(), b"first");
        assert_eq!(std::fs::read(uploads.join("clip-1.mp4")).unwrap(), b"second");
        assert_eq!(std::fs::read(uploads.join("clip-2.mp4")).unwrap(), b"third");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 非法文件名上传 → 清洗后可落盘（路径穿越文件名被剥成 basename）
    #[tokio::test]
    async fn upload_input_sanitizes_malicious_file_name() {
        let root = unique_root("input-sanitize");
        let state = test_state(root.clone());
        let app = router().with_state(state);

        let mut body = Vec::new();
        form_part(&mut body, "file", Some("../../evil.sh"), b"payload");
        finish_multipart(&mut body);

        let resp = app
            .oneshot(upload_request("/upload/input", body))
            .await
            .unwrap();
        let (status, json) = response_json(resp).await;
        assert_eq!(status, StatusCode::OK, "响应: {json}");

        // 落点必须在 workspace/uploads 内，绝不越狱
        let path = PathBuf::from(json["path"].as_str().unwrap());
        assert!(
            path.starts_with(root.join("workspace").join("uploads")),
            "落点越狱: {}",
            path.display()
        );
        assert_eq!(path.file_name().unwrap(), "evil.sh");
        assert!(!root.join("evil.sh").exists());
        assert!(!root.join("workspace/evil.sh").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 缺 file 字段 → 400（新键待落盘：回退为键本身）
    #[tokio::test]
    async fn upload_input_missing_file_field_400() {
        let root = unique_root("input-nofile");
        let state = test_state(root.clone());
        let app = router().with_state(state);

        let mut body = Vec::new();
        form_part(&mut body, "not-file", Some("x.bin"), b"data");
        finish_multipart(&mut body);

        let resp = app
            .oneshot(upload_request("/upload/input", body))
            .await
            .unwrap();
        let (status, json) = response_json(resp).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(json["error"], "apiModels.inputUploadMissingFile");
        // uploads 目录不应被创建出任何文件
        let uploads = root.join("workspace/uploads");
        let count = std::fs::read_dir(&uploads)
            .map(|mut e| e.next().is_some())
            .unwrap_or(false);
        assert!(!count, "uploads 目录不应有残留文件");
        let _ = std::fs::remove_dir_all(&root);
    }
}
