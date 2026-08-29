//! 整合包（Pack）管理 API — Wave 2 **B2 (DaemonPacks)** 实现。
//!
//! # 冻结契约（`docs/PACK_UNIFY_PLAN.md` §8.1，共 7 条路由）
//!
//! | 方法+路径 | 语义 |
//! |---|---|
//! | GET /api/packs | 已装包列表（注册表） |
//! | POST /api/packs/import | `{source:"local",path}` \| `{source:"url",url}` → 202 `{pack_id}`，进度走 WS |
//! | POST /api/packs/upload | multipart `.zip`（字段名 `file`，仲裁 #3）→ 202 同上 |
//! | GET /api/packs/{id} | 详情（注册条目 + 逐模型适配报告 §4.6） |
//! | DELETE /api/packs/{id} | `?keep_models=true` 卸载（模型可选保留） |
//! | POST /api/packs/build | 圈选模型+管线 → 202 → 构建完成可下载 |
//! | GET /api/packs/{id}/export | `.zip` 下载（302 → /api/pack-files 流式通道） |
//!
//! # 架构要点
//!
//! - **导入编排**：后台任务（`tokio::spawn` + `spawn_blocking`）委托
//!   [`ep_pack::import::import_pack`]（B1 编排核心，§4.4 全流程）；
//!   daemon 侧负责来源解析（local/url/upload）、URL 下载（curl +
//!   config.network 代理注入）、模块解析回调（查模块 manifest）、
//!   进度回调 → WS、reference 模型后台下载驱动（下载完成后回填 meta
//!   的 pack_id/qualified_id/合并 tags，使 DELETE 的 meta.pack_id
//!   扫描覆盖 reference 后下载模型）。
//! - **注册表**（§4.4）：`runtime/packs/<pack-id>.json`，B1 的
//!   [`ep_pack::import::InstalledPack`] 为唯一持久形状（原子写、
//!   `list_installed_packs` 读取）——本文件不再维护内存镜像，
//!   磁盘即单一事实源。
//! - **WS 进度**：[`crate::state::WsMessage::PackImport`]（形状对齐前端
//!   `WsPackImportMessage`，仲裁 #3）；stage 直传 B1 小写阶段名
//!   （extracting/verifying/manifest/models/pipelines/registering），
//!   state 由本文件包络 running → completed/failed；经通用 WsMessage
//!   通道 `state.model_download_tx` 投递到 GET /ws。
//! - **构建/导出**（§4.5）：daemon 侧组装包内容目录（模型圈选 → 暂存布局）
//!   后调 [`ep_pack::build::build_pack`]；产物缓存 `runtime/pack-out/` 供
//!   export 下载。已装包导出无缓存产物时按注册表内容即时重建。
//! - **错误响应**：统一 `err_response` + i18n 键（`packs:error*`，键集见
//!   `reports/i18n_key_requests.md`；键缺失时 i18n 回退为键名本身）。
//!
//! # 双平台纪律
//!
//! 路径一律 `Path::join`；multipart 临时文件与 URL 下载落
//! `std::env::temp_dir()`；注册表 JSON 由 B1 原子写（临时文件 + rename）。

use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::multipart::MultipartRejection;
use axum::extract::{DefaultBodyLimit, Multipart, Path as UrlPath, Query, State};
use axum::http::{header, HeaderValue, Request, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;
use tower_http::services::ServeDir;
use tracing::{debug, info, warn};

use ep_core::model::{DownloadHandle, DownloadProgress, DownloadState, ModelManager};
use ep_core::model_id::{PinnedModelId, QualifiedId};
use ep_core::module::manifest::{ModelDecl, ModelSource, ModuleManifest};
use ep_core::types::{ComputeBackend, ComputeDevice};
use ep_pack::build::{build_pack as build_pack_archive, BuildError, BuildPlan};
use ep_pack::import::{
    adapt_model, import_pack as run_import_core, list_installed_packs, read_installed_pack,
    registry_entry_path, AdaptationVerdict, ImportError, ImportOptions, ImportTargets,
    InstalledPack, PackImportProgress, PendingDownload, PendingDownloadRequest, ResolvedModel,
};
use ep_pack::manifest::{semver, ModelMode, PackManifest, PackModelEntry, PackPipelineRef};

use crate::api::err_response;
use crate::api::pipelines::pipeline_bridge;
use crate::state::{AppState, DownloadEntry, WsMessage};

// ─── 常量与路径 ─────────────────────────────────────────────────────────────

/// 当前 daemon 版本（min_ep_version 门禁输入，与 ep-pack 工作区版本一致）
const EP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// URL 下载整合包的 curl 超时（秒）：连接超时 30s，总时长 1h（GB 级包）
const DOWNLOAD_CONNECT_TIMEOUT_SECS: &str = "30";
const DOWNLOAD_MAX_TIME_SECS: &str = "3600";

/// 已装包注册表目录（§4.4 runtime/packs/；与 ImportTargets::from_root 一致，
/// 独立于 config.packs.staging_dir）
fn registry_dir(root: &Path) -> PathBuf {
    root.join("runtime").join("packs")
}

/// 构建产物缓存目录（export 下载来源，任务书约定 runtime/pack-out/）
fn pack_out_dir(root: &Path) -> PathBuf {
    root.join("runtime").join("pack-out")
}

/// 导入/构建暂存根目录：`config.packs.staging_dir`（相对路径基于 root）
fn staging_root(root: &Path, configured: &str) -> PathBuf {
    let p = Path::new(configured);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        root.join(p)
    }
}

/// 唯一暂存 id（不引入 uuid crate：纳秒时间戳 + 进程内序号 + pid）
fn unique_id() -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{nanos:x}-{seq:04x}-{}", std::process::id())
}

/// pack id 文件名安全校验：严格 `<publisher>.<pack-name>` 语法（§4.2）。
/// URL 路径参数作文件名使用前必须过本检查（拒绝路径分隔符/`..` 等）。
fn is_safe_pack_id(id: &str) -> bool {
    let segments: Vec<&str> = id.split('.').collect();
    segments.len() == 2 && segments.iter().all(|seg| is_id_segment(seg))
}

fn is_id_segment(seg: &str) -> bool {
    let mut chars = seg.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

// ─── 错误表示（i18n 键 + 插值参数）─────────────────────────────────────────

struct PackApiError {
    status: StatusCode,
    key: &'static str,
    params: Vec<(&'static str, String)>,
}

impl PackApiError {
    fn new(status: StatusCode, key: &'static str) -> Self {
        Self {
            status,
            key,
            params: Vec::new(),
        }
    }

    fn with_params(
        status: StatusCode,
        key: &'static str,
        params: Vec<(&'static str, String)>,
    ) -> Self {
        Self {
            status,
            key,
            params,
        }
    }

    /// 便捷构造：携带单个 {{detail}} 插值（底层技术错误透传）
    fn detail(status: StatusCode, key: &'static str, detail: impl std::fmt::Display) -> Self {
        Self::with_params(status, key, vec![("detail", detail.to_string())])
    }
}

async fn pack_err(state: &Arc<AppState>, e: PackApiError) -> (StatusCode, Json<Value>) {
    err_response(state, e.status, e.key, &e.params).await
}

/// 阻塞上下文（后台任务）内的错误表示：i18n 键 + 参数（最终经 WS 呈现）
struct PackTaskError {
    key: &'static str,
    params: Vec<(&'static str, String)>,
}

impl PackTaskError {
    fn new(key: &'static str) -> Self {
        Self {
            key,
            params: Vec::new(),
        }
    }

    fn with_params(key: &'static str, params: Vec<(&'static str, String)>) -> Self {
        Self { key, params }
    }

    fn detail(key: &'static str, detail: impl std::fmt::Display) -> Self {
        Self::with_params(key, vec![("detail", detail.to_string())])
    }
}

impl From<PackTaskError> for PackApiError {
    fn from(e: PackTaskError) -> Self {
        PackApiError::with_params(StatusCode::BAD_REQUEST, e.key, e.params)
    }
}

/// 解包错误 → i18n 键映射（键见 `reports/i18n_key_requests.md` A4 段）
fn extract_failure(e: &ep_pack::extract::ExtractError) -> PackTaskError {
    use ep_pack::extract::ExtractError;
    match e {
        ExtractError::Open { path, source } => PackTaskError::detail(
            "packs:errorArchiveOpen",
            format!("{}: {source}", path.display()),
        ),
        ExtractError::Parse(source) => {
            PackTaskError::detail("packs:errorArchiveInvalid", source)
        }
        ExtractError::UnsafePath(name) => {
            PackTaskError::with_params("packs:errorUnsafePath", vec![("entry", name.clone())])
        }
        ExtractError::SymlinkEntry(name) => PackTaskError::with_params(
            "packs:errorSymlinkEntry",
            vec![("entry", name.clone())],
        ),
        ExtractError::SymlinkEscape(name) => PackTaskError::with_params(
            "packs:errorSymlinkEscape",
            vec![("entry", name.clone())],
        ),
        ExtractError::SpecialFileEntry { name, mode } => PackTaskError::with_params(
            "packs:errorSpecialFile",
            vec![("entry", name.clone()), ("mode", format!("{mode:o}"))],
        ),
        ExtractError::DuplicateEntry(name) => PackTaskError::with_params(
            "packs:errorDuplicateEntry",
            vec![("entry", name.clone())],
        ),
        ExtractError::MissingManifest => PackTaskError::new("packs:errorMissingManifest"),
        ExtractError::SizeLimitExceeded { limit } => PackTaskError::with_params(
            "packs:errorSizeLimit",
            vec![("limit", limit.to_string())],
        ),
        other => PackTaskError::detail("packs:errorInternal", other),
    }
}

/// 校验和错误 → i18n 键映射
fn checksum_failure(e: &ep_pack::checksum::ChecksumError) -> PackTaskError {
    use ep_pack::checksum::ChecksumError;
    match e {
        ChecksumError::ChecksumsFileMissing { .. } => {
            PackTaskError::new("packs:errorChecksumMissing")
        }
        ChecksumError::Parse { source } => {
            PackTaskError::detail("packs:errorChecksumParse", source)
        }
        ChecksumError::Integrity(report) => PackTaskError::with_params(
            "packs:errorChecksumIntegrity",
            vec![
                ("missing", report.missing.len().to_string()),
                ("unexpected", report.unexpected.len().to_string()),
                ("mismatched", report.mismatched.len().to_string()),
            ],
        ),
        other => PackTaskError::detail("packs:errorInternal", other),
    }
}

/// B1 导入硬失败 → i18n 键映射（WS failed 消息文案来源）
fn import_failure(e: &ImportError) -> PackTaskError {
    match e {
        ImportError::Extract(x) => extract_failure(x),
        ImportError::Checksum(x) => checksum_failure(x),
        ImportError::Manifest(x) => PackTaskError::detail("packs:errorManifestInvalid", x),
        ImportError::MinEpVersion { required, current } => PackTaskError::with_params(
            "packs:errorMinVersion",
            vec![("min", required.clone()), ("current", current.clone())],
        ),
        ImportError::BadVersion { detail } => {
            PackTaskError::detail("packs:errorManifestInvalid", detail)
        }
        ImportError::PackAlreadyInstalled { pack_id, .. } => PackTaskError::with_params(
            "packs:errorAlreadyInstalled",
            vec![("id", pack_id.clone())],
        ),
        ImportError::ModelConflict { target, .. } => PackTaskError::with_params(
            "packs:errorModelConflict",
            vec![("target", target.display().to_string())],
        ),
        ImportError::BundleMissing {
            qualified_id,
            variant,
            target_dir,
        } => PackTaskError::with_params(
            "packs:errorBundleMissing",
            vec![
                ("model", format!("{qualified_id}@{variant}")),
                ("target_dir", target_dir.clone()),
            ],
        ),
        ImportError::PipelineFileMissing { file } => PackTaskError::with_params(
            "packs:errorPipelineInvalid",
            vec![("file", file.clone()), ("detail", "file missing in archive".to_string())],
        ),
        ImportError::InvalidPipeline { file, detail } => PackTaskError::with_params(
            "packs:errorPipelineInvalid",
            vec![("file", file.clone()), ("detail", detail.clone())],
        ),
        ImportError::Io { path, source } => PackTaskError::detail(
            "packs:errorInternal",
            format!("{}: {source}", path.display()),
        ),
        ImportError::Json { path, source } => PackTaskError::detail(
            "packs:errorInternal",
            format!("{}: {source}", path.display()),
        ),
    }
}

// ─── 路由 ───────────────────────────────────────────────────────────────────

/// DELETE /api/packs/{id} 查询参数：`?keep_models=true` → 卸载时保留模型文件。
#[derive(Debug, Clone, Deserialize)]
pub struct DeletePackQuery {
    #[serde(default)]
    pub keep_models: bool,
}

/// POST /api/packs/import 请求体（§8.1：`{source:"local",path}` | `{source:"url",url}`；
/// serde 形状与 B1 [`ep_pack::import::ImportSource`] 一致）
#[derive(Debug, Deserialize)]
#[serde(tag = "source", rename_all = "lowercase")]
enum ImportRequest {
    Local { path: String },
    Url { url: String },
}

/// POST /api/packs/build 请求体（§8.1：models/pipelines/bundle/tags）。
///
/// `id`/`name`/`version`/`description` 为冻结契约之外的**可选**扩展字段
/// （缺省时自动生成包身份）；serde 对未知字段宽容，不影响契约形状消费。
#[derive(Debug, Deserialize)]
struct BuildRequest {
    /// 圈选模型 pin 列表（`<qualified_id>@<variant>`，§4.3）
    #[serde(default)]
    models: Vec<String>,
    /// 打包携带的管线 id 列表
    #[serde(default)]
    pipelines: Vec<String>,
    /// 以 bundle 模式携带权重的 qualified_id 列表
    #[serde(default)]
    bundle: Vec<String>,
    /// 按 tag 圈选模型（与 models 并集，§4.5 tag 组装闭环）
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    description: Option<String>,
}

/// `/api/packs/*` 路由表（挂载于 [`crate::api::api_router`]）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/packs", get(list_packs))
        .route("/packs/import", post(import_pack))
        .route("/packs/build", post(build_pack))
        .route("/packs/{id}", get(get_pack).delete(delete_pack))
        .route("/packs/{id}/export", get(export_pack))
        // upload 路由单独关闭默认 2MB body 上限（整合包可达数 GB，
        // 模式同 api/upload.rs；layer 仅作用于本 merge 分支）
        .merge(
            Router::new()
                .route("/packs/upload", post(upload_pack))
                .layer(DefaultBodyLimit::disable()),
        )
        // 导出产物流式下载通道（export handler 302 到此处，ServeDir 承接）
        .nest("/pack-files", pack_files_router())
}

/// ServeDir 根 = `{root}/runtime/pack-out`（构建产物缓存目录）。
///
/// 路由构造先于 AppState 注入，无法读取运行期状态，故按启动同款方式
/// （resolve_root）解析——与 tasks.rs 的 task-files 通道同款处理。
fn pack_files_router() -> Router<Arc<AppState>> {
    let root = ep_core::config::resolve_root();
    Router::new()
        .fallback_service(ServeDir::new(pack_out_dir(&root)))
        .layer(middleware::from_fn(attachment_disposition))
}

/// 为 ServeDir 的成功响应补 `Content-Disposition: attachment; filename=...`
/// （模式同 api/tasks.rs；文件名取 URI 最后一段，RFC 5987 双形式）。
async fn attachment_disposition(req: Request<axum::body::Body>, next: Next) -> Response {
    let raw_name = req
        .uri()
        .path()
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_string();
    let mut resp = next.run(req).await;
    if resp.status().is_success() && !raw_name.is_empty() {
        let value = format!("attachment; filename=\"{raw_name}\"; filename*=UTF-8''{raw_name}");
        if let Ok(v) = HeaderValue::from_str(&value) {
            resp.headers_mut().insert(header::CONTENT_DISPOSITION, v);
        }
    }
    resp
}

// ─── 响应形状（前端 PackInfo / PackDetail / PackAdaptationEntry 契约）─────

/// 列表/详情输出的模型条目（前端 `PackModelRef` 形状）
#[derive(Debug, Serialize)]
struct PackModelRefOut {
    qualified_id: String,
    variant: String,
    /// "reference" | "bundle"
    mode: &'static str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tags: Vec<String>,
}

/// GET /api/packs 列表项 / 详情基础（前端 `PackInfo` 形状：
/// name 必有——注册表缺失时回退 id，仲裁转发约定）。
#[derive(Debug, Serialize)]
struct PackInfoOut {
    id: String,
    version: String,
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    installed_at: String,
    models: Vec<PackModelRefOut>,
    pipelines: Vec<String>,
}

fn pack_info_out(p: &InstalledPack) -> PackInfoOut {
    PackInfoOut {
        id: p.id.clone(),
        version: p.version.clone(),
        name: p.name.clone().unwrap_or_else(|| p.id.clone()),
        description: p.description.clone(),
        installed_at: p.installed_at.clone(),
        models: p
            .models
            .iter()
            .map(|m| PackModelRefOut {
                qualified_id: m.qualified_id.clone(),
                variant: m.variant.clone(),
                mode: m.mode.as_str(),
                tags: m.tags.clone(),
            })
            .collect(),
        pipelines: p.pipelines.clone(),
    }
}

/// 逐模型适配结论（前端 `PackAdaptationEntry` 契约，仲裁 #3/#4：
/// 库层 `{verdict, reason}` → `{ok, device, note}`，note 走 i18n）。
#[derive(Debug, Serialize)]
struct AdaptationOut {
    qualified_id: String,
    variant: String,
    /// verdict != unsupported
    ok: bool,
    /// 结论设备（如 "cuda:0"）；None = CPU 保底或不支持（契约 null 语义）
    #[serde(skip_serializing_if = "Option::is_none")]
    device: Option<String>,
    note: String,
}

// ─── Handlers ───────────────────────────────────────────────────────────────

/// GET /api/packs — 已装包列表（B1 注册表，按 id 排序）。
async fn list_packs(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    let dir = registry_dir(&state.root);
    let packs = match tokio::task::spawn_blocking(move || list_installed_packs(&dir)).await {
        Ok(Ok(p)) => p,
        Ok(Err(e)) => {
            return pack_err(
                &state,
                PackApiError::detail(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "packs:errorInternal",
                    e,
                ),
            )
            .await
        }
        Err(join) => {
            return pack_err(
                &state,
                PackApiError::detail(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "packs:errorInternal",
                    join,
                ),
            )
            .await
        }
    };
    let mut packs = packs;
    packs.sort_by(|a, b| a.id.cmp(&b.id));
    let out: Vec<PackInfoOut> = packs.iter().map(pack_info_out).collect();
    (StatusCode::OK, Json(json!(out)))
}

/// GET /api/packs/{id} — 详情：注册条目 + 逐模型适配报告（§4.6 实时计算）。
async fn get_pack(
    State(state): State<Arc<AppState>>,
    UrlPath(id): UrlPath<String>,
) -> (StatusCode, Json<Value>) {
    if !is_safe_pack_id(&id) {
        return pack_err(
            &state,
            PackApiError::with_params(
                StatusCode::NOT_FOUND,
                "packs:errorNotFound",
                vec![("id", id)],
            ),
        )
        .await;
    }
    let reg_path = registry_entry_path(&registry_dir(&state.root), &id);
    let installed = match read_installed_pack(&reg_path) {
        Ok(Some(p)) => p,
        Ok(None) => {
            return pack_err(
                &state,
                PackApiError::with_params(
                    StatusCode::NOT_FOUND,
                    "packs:errorNotFound",
                    vec![("id", id)],
                ),
            )
            .await
        }
        Err(e) => {
            return pack_err(
                &state,
                PackApiError::detail(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "packs:errorInternal",
                    e,
                ),
            )
            .await
        }
    };

    let devices = state.devices.read().await.clone();
    let manifests = read_manifests(&state).await;
    let lang = state.lang().await;

    let mut detail = serde_json::to_value(pack_info_out(&installed))
        .expect("PackInfoOut serializes");
    detail["adaptation"] = serde_json::to_value(live_adaptation(
        &devices,
        &manifests,
        &installed.models,
        &lang,
    ))
    .expect("adaptation serializes");
    (StatusCode::OK, Json(detail))
}

/// POST /api/packs/import — 从本地路径 / URL 导入整合包（202 + WS 进度）。
///
/// 契约要求 202 响应即携带 `pack_id`，故 handler 先同步快读清单
/// （zip 中央目录在文件尾部，GB 级归档亦毫秒级）并做重复安装预检
/// （409）；URL 来源需先下载才能读清单，下载完成后才 202。
async fn import_pack(
    State(state): State<Arc<AppState>>,
    body: Result<Json<ImportRequest>, axum::extract::rejection::JsonRejection>,
) -> (StatusCode, Json<Value>) {
    let Json(req) = match body {
        Ok(b) => b,
        Err(e) => {
            return pack_err(
                &state,
                PackApiError::detail(
                    StatusCode::BAD_REQUEST,
                    "packs:errorImportRequestInvalid",
                    e,
                ),
            )
            .await
        }
    };

    match req {
        ImportRequest::Local { path } => {
            let archive = PathBuf::from(&path);
            if !archive.is_file() {
                return pack_err(
                    &state,
                    PackApiError::with_params(
                        StatusCode::BAD_REQUEST,
                        "packs:errorImportFileMissing",
                        vec![("path", path)],
                    ),
                )
                .await;
            }
            let pack_id = match spawn_peek(&archive).await {
                Ok(id) => id,
                Err(fail) => return pack_err(&state, fail.into()).await,
            };
            if let Some(resp) = reject_if_installed(&state, &pack_id).await {
                return resp;
            }
            spawn_import_task(state, archive, false, pack_id.clone());
            (StatusCode::ACCEPTED, Json(json!({ "pack_id": pack_id })))
        }
        ImportRequest::Url { url } => {
            let url = url.trim().to_string();
            if !(url.starts_with("http://") || url.starts_with("https://")) {
                return pack_err(
                    &state,
                    PackApiError::detail(
                        StatusCode::BAD_REQUEST,
                        "packs:errorImportRequestInvalid",
                        "url must start with http:// or https://",
                    ),
                )
                .await;
            }
            // 契约：202 即带 pack_id → 下载必须先行（后台任务只跑导入剩余流程）
            let network_env = state.config.read().await.network.env_vars();
            let temp = std::env::temp_dir().join(format!("ep-pack-dl-{}.pack", unique_id()));
            let dl_temp = temp.clone();
            let dl_url = url.clone();
            let download = tokio::task::spawn_blocking(move || {
                download_archive(&dl_url, &dl_temp, &network_env)
            })
            .await;
            match download {
                Ok(Ok(())) => {}
                Ok(Err(fail)) => {
                    let _ = tokio::fs::remove_file(&temp).await;
                    return pack_err(&state, fail.into()).await;
                }
                Err(join) => {
                    let _ = tokio::fs::remove_file(&temp).await;
                    return pack_err(
                        &state,
                        PackApiError::detail(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "packs:errorInternal",
                            join,
                        ),
                    )
                    .await;
                }
            }
            let pack_id = match spawn_peek(&temp).await {
                Ok(id) => id,
                Err(fail) => {
                    let _ = tokio::fs::remove_file(&temp).await;
                    return pack_err(&state, fail.into()).await;
                }
            };
            if let Some(resp) = reject_if_installed(&state, &pack_id).await {
                let _ = tokio::fs::remove_file(&temp).await;
                return resp;
            }
            spawn_import_task(state, temp, true, pack_id.clone());
            (StatusCode::ACCEPTED, Json(json!({ "pack_id": pack_id })))
        }
    }
}

/// POST /api/packs/upload — multipart `.zip` 上传导入（字段名 `file`）。
///
/// 临时落盘走 `std::env::temp_dir()`（双平台硬约束），逐 chunk 流式写盘；
/// 成功后转后台导入任务（与 import 同流程），临时归档由后台任务清理。
async fn upload_pack(
    State(state): State<Arc<AppState>>,
    multipart: Result<Multipart, MultipartRejection>,
) -> (StatusCode, Json<Value>) {
    let mut multipart = match multipart {
        Ok(m) => m,
        Err(e) => {
            warn!(error = %e, "pack upload multipart rejected");
            return pack_err(
                &state,
                PackApiError::new(StatusCode::BAD_REQUEST, "packs:errorUploadNoFile"),
            )
            .await;
        }
    };

    let temp = std::env::temp_dir().join(format!("ep-pack-upload-{}.pack", unique_id()));
    let mut wrote_file = false;

    while let Ok(Some(mut field)) = multipart.next_field().await {
        if field.name() != Some("file") {
            continue; // 未知字段忽略（multer 自动跳过未读内容）
        }
        let mut file = match tokio::fs::File::create(&temp).await {
            Ok(f) => f,
            Err(e) => {
                return pack_err(
                    &state,
                    PackApiError::detail(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "packs:errorInternal",
                        e,
                    ),
                )
                .await
            }
        };
        while let Ok(Some(chunk)) = field.chunk().await {
            if file.write_all(&chunk).await.is_err() {
                let _ = tokio::fs::remove_file(&temp).await;
                return pack_err(
                    &state,
                    PackApiError::new(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "packs:errorInternal",
                    ),
                )
                .await;
            }
        }
        wrote_file = true;
        break; // 只取第一个 file 字段
    }

    if !wrote_file {
        let _ = tokio::fs::remove_file(&temp).await;
        return pack_err(
            &state,
            PackApiError::new(StatusCode::BAD_REQUEST, "packs:errorUploadNoFile"),
        )
        .await;
    }

    let pack_id = match spawn_peek(&temp).await {
        Ok(id) => id,
        Err(fail) => {
            let _ = tokio::fs::remove_file(&temp).await;
            return pack_err(&state, fail.into()).await;
        }
    };
    if let Some(resp) = reject_if_installed(&state, &pack_id).await {
        let _ = tokio::fs::remove_file(&temp).await;
        return resp;
    }
    info!(pack_id = %pack_id, "API: pack upload accepted");
    spawn_import_task(state, temp, true, pack_id.clone());
    (StatusCode::ACCEPTED, Json(json!({ "pack_id": pack_id })))
}

/// DELETE /api/packs/{id} — 卸载整合包（先卸载再导入为 §4.4 裁定流程）。
///
/// `keep_models=false`（默认）：删除 `meta.pack_id` 指向本包的模型目录
/// （bundle 落位时写入）；管线删除本包实际安装的条目（按 `[pipeline].id`
/// 反查文件）；注册条目删除。响应 `{ok: true}`（前端契约）。
async fn delete_pack(
    State(state): State<Arc<AppState>>,
    UrlPath(id): UrlPath<String>,
    Query(query): Query<DeletePackQuery>,
) -> (StatusCode, Json<Value>) {
    if !is_safe_pack_id(&id) {
        return pack_err(
            &state,
            PackApiError::with_params(
                StatusCode::NOT_FOUND,
                "packs:errorNotFound",
                vec![("id", id)],
            ),
        )
        .await;
    }
    let reg_path = registry_entry_path(&registry_dir(&state.root), &id);
    let installed = match read_installed_pack(&reg_path) {
        Ok(Some(p)) => p,
        Ok(None) => {
            return pack_err(
                &state,
                PackApiError::with_params(
                    StatusCode::NOT_FOUND,
                    "packs:errorNotFound",
                    vec![("id", id)],
                ),
            )
            .await
        }
        Err(e) => {
            return pack_err(
                &state,
                PackApiError::detail(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "packs:errorInternal",
                    e,
                ),
            )
            .await
        }
    };

    // 模型删除（重活在注册表文件删除前执行，失败不阻断卸载）
    if !query.keep_models {
        let mgr = build_model_manager(&state).await;
        for model in mgr.list_downloaded_models() {
            if model.meta.pack_id.as_deref() == Some(installed.id.as_str()) {
                let dir = mgr.model_dir(&model.target_dir);
                if let Err(e) = tokio::fs::remove_dir_all(&dir).await {
                    warn!(dir = %dir.display(), error = %e, "failed to remove pack model dir");
                }
            }
        }
    }
    // 管线删除：按已装 id 反查 config/pipelines/*.toml
    let pipelines_dir = state.root.join("config").join("pipelines");
    for (path, spec) in scan_pipeline_specs(&pipelines_dir) {
        if installed.pipelines.contains(&spec.pipeline.id) {
            if let Err(e) = tokio::fs::remove_file(&path).await {
                warn!(file = %path.display(), error = %e, "failed to remove pack pipeline");
            } else {
                info!(pipeline = %spec.pipeline.id, "removed pipeline installed by pack");
            }
        }
    }
    if let Err(e) = tokio::fs::remove_file(&reg_path).await {
        if e.kind() != std::io::ErrorKind::NotFound {
            warn!(path = %reg_path.display(), error = %e, "failed to remove pack registry file");
        }
    }

    info!(pack_id = %installed.id, keep_models = query.keep_models, "API: pack deleted");
    (StatusCode::OK, Json(json!({ "ok": true })))
}

/// POST /api/packs/build — 圈选模型（pin/tag）+ 管线 → 202 后台构建。
///
/// 构建完成产物缓存 `runtime/pack-out/<id>-<version>.zip`，
/// 经 GET /api/packs/{id}/export 下载；完成/失败经 WS pack_import
/// （stage="build"）广播。
async fn build_pack(
    State(state): State<Arc<AppState>>,
    body: Result<Json<BuildRequest>, axum::extract::rejection::JsonRejection>,
) -> (StatusCode, Json<Value>) {
    let Json(req) = match body {
        Ok(b) => b,
        Err(e) => {
            return pack_err(
                &state,
                PackApiError::detail(StatusCode::BAD_REQUEST, "packs:errorBuildInvalid", e),
            )
            .await
        }
    };

    match prepare_build_job(&state, req).await {
        Ok(job) => {
            let pack_id = job.pack_id.clone();
            spawn_build_task(state, job);
            (StatusCode::ACCEPTED, Json(json!({ "pack_id": pack_id })))
        }
        Err(e) => pack_err(&state, e).await,
    }
}

/// GET /api/packs/{id}/export — `.zip` 下载。
///
/// 产物已缓存 → 302 到 `/api/pack-files/<filename>`（ServeDir 流式）；
/// 已装包无缓存产物 → 按注册表内容即时重建；均不满足 → 404。
async fn export_pack(
    State(state): State<Arc<AppState>>,
    UrlPath(id): UrlPath<String>,
) -> Response {
    if !is_safe_pack_id(&id) {
        return pack_err(
            &state,
            PackApiError::with_params(
                StatusCode::NOT_FOUND,
                "packs:errorNotFound",
                vec![("id", id)],
            ),
        )
        .await
        .into_response();
    }

    match ensure_export_artifact(&state, &id).await {
        Ok(file_name) => {
            let location = format!("/api/pack-files/{file_name}");
            (StatusCode::FOUND, [(header::LOCATION, location)]).into_response()
        }
        Err(e) => pack_err(&state, e).await.into_response(),
    }
}

// ─── 共享辅助（handler 层）──────────────────────────────────────────────────

/// 读取全部模块 manifest（克隆出锁外使用）
async fn read_manifests(state: &AppState) -> Vec<ModuleManifest> {
    state
        .modules
        .read()
        .await
        .iter()
        .filter_map(|m| m.manifest.clone())
        .collect()
}

/// 从 AppState 构建 ModelManager（附带全部 manifest，与 api/upload.rs 同款）
async fn build_model_manager(state: &AppState) -> ModelManager {
    let config = state.config.read().await;
    let manifests = read_manifests(state).await;
    ModelManager::new(&config.models, &state.root).with_manifests(manifests)
}

/// 重复安装预检（B1 语义 PackAlreadyInstalled 硬失败 → API 层 409 提前拦截）
async fn reject_if_installed(
    state: &Arc<AppState>,
    pack_id: &str,
) -> Option<(StatusCode, Json<Value>)> {
    let reg_path = registry_entry_path(&registry_dir(&state.root), pack_id);
    match read_installed_pack(&reg_path) {
        Ok(Some(_)) => Some(
            pack_err(
                state,
                PackApiError::with_params(
                    StatusCode::CONFLICT,
                    "packs:errorAlreadyInstalled",
                    vec![("id", pack_id.to_string())],
                ),
            )
            .await,
        ),
        Ok(None) => None,
        Err(e) => Some(
            pack_err(
                state,
                PackApiError::detail(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "packs:errorInternal",
                    e,
                ),
            )
            .await,
        ),
    }
}

/// spawn_blocking 快读归档清单（peek pack_id + 校验 + min_ep_version 门禁）
async fn spawn_peek(archive: &Path) -> Result<String, PackTaskError> {
    let path = archive.to_path_buf();
    tokio::task::spawn_blocking(move || peek_pack_id(&path))
        .await
        .map_err(|e| PackTaskError::detail("packs:errorInternal", e))?
}

/// 从 zip 归档读取 `ep-pack.toml` 并解析为清单（解出单条目到临时目录，
/// 经 ep-pack 解析器解析；ep-daemon 无 toml 依赖）。
fn read_manifest_from_archive(archive: &Path) -> Result<PackManifest, PackTaskError> {
    let file = std::fs::File::open(archive).map_err(|e| {
        PackTaskError::detail(
            "packs:errorArchiveOpen",
            format!("{}: {e}", archive.display()),
        )
    })?;
    let mut zip = zip::ZipArchive::new(BufReader::new(file))
        .map_err(|e| PackTaskError::detail("packs:errorArchiveInvalid", e))?;
    let mut entry = zip
        .by_name(ep_pack::extract::MANIFEST_FILE_NAME)
        .map_err(|_| PackTaskError::new("packs:errorMissingManifest"))?;
    let mut text = String::new();
    std::io::Read::read_to_string(&mut entry, &mut text)
        .map_err(|e| PackTaskError::detail("packs:errorArchiveInvalid", e))?;
    drop(entry);
    drop(zip);

    // 临时落盘后经 from_file 解析（复用 ep-pack 解析器）
    let peek_dir = std::env::temp_dir().join(format!("ep-pack-peek-{}", unique_id()));
    let manifest_path = peek_dir.join(ep_pack::extract::MANIFEST_FILE_NAME);
    let parsed = (|| -> Result<PackManifest, PackTaskError> {
        std::fs::create_dir_all(&peek_dir)
            .map_err(|e| PackTaskError::detail("packs:errorInternal", e))?;
        std::fs::write(&manifest_path, &text)
            .map_err(|e| PackTaskError::detail("packs:errorInternal", e))?;
        PackManifest::from_file(&manifest_path)
            .map_err(|e| PackTaskError::detail("packs:errorManifestInvalid", e))
    })();
    let _ = std::fs::remove_dir_all(&peek_dir);
    parsed
}

/// 快读 zip 内 `ep-pack.toml`：解析 + 校验 + min_ep_version 门禁 →
/// 返回 pack.id（202 响应携带，后台任务前置拦截）。
fn peek_pack_id(archive: &Path) -> Result<String, PackTaskError> {
    let manifest = read_manifest_from_archive(archive)?;

    if let Err(errors) = manifest.validate() {
        return Err(PackTaskError::with_params(
            "packs:errorManifestInvalid",
            vec![("detail", errors.join("; "))],
        ));
    }
    if let Some(min) = &manifest.pack.min_ep_version {
        let ok = semver::satisfies_min(EP_VERSION, min)
            .map_err(|e| PackTaskError::detail("packs:errorManifestInvalid", e))?;
        if !ok {
            return Err(PackTaskError::with_params(
                "packs:errorMinVersion",
                vec![("min", min.clone()), ("current", EP_VERSION.to_string())],
            ));
        }
    }
    if !is_safe_pack_id(&manifest.pack.id) {
        return Err(PackTaskError::with_params(
            "packs:errorManifestInvalid",
            vec![("detail", format!("unsafe pack.id `{}`", manifest.pack.id))],
        ));
    }
    Ok(manifest.pack.id)
}

/// URL 下载整合包归档：curl（Windows 10+ / Linux 均自带），
/// 注入 config.network 代理环境变量（curl 原生识别 HTTP(S)_PROXY）。
fn download_archive(
    url: &str,
    dest: &Path,
    proxy_env: &[(String, String)],
) -> Result<(), PackTaskError> {
    let mut cmd = std::process::Command::new("curl");
    cmd.args([
        "-sSL",
        "--fail",
        "--location",
        "--connect-timeout",
        DOWNLOAD_CONNECT_TIMEOUT_SECS,
        "--max-time",
        DOWNLOAD_MAX_TIME_SECS,
        "-o",
    ])
    .arg(dest)
    .arg(url);
    for (k, v) in proxy_env {
        cmd.env(k, v);
    }
    let output = cmd
        .output()
        .map_err(|e| PackTaskError::detail("packs:errorDownloadFailed", e))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        return Err(PackTaskError::with_params(
            "packs:errorDownloadFailed",
            vec![("detail", detail.trim().to_string())],
        ));
    }
    Ok(())
}

// ─── 模块解析回调（B1 resolve 契约）与适配报告（§4.6）─────────────────────

/// 按 qualified_id + variant 在模块清单中解析模型声明。
///
/// 匹配规则（§4.3）：`decl.qualified_id` 规范形 == 条目 qualified_id，
/// 且 `decl.id` == 条目 variant（变体维度与模块 `[[models]].id` 对应）。
fn resolve_entry(
    manifests: &[ModuleManifest],
    entry: &PackModelEntry,
) -> Result<ResolvedModel, String> {
    for mf in manifests {
        for decl in &mf.models {
            let Some(q) = decl.qualified_id.as_deref() else {
                continue;
            };
            let Ok(parsed) = QualifiedId::parse(q) else {
                continue;
            };
            if parsed.to_canonical() != entry.qualified_id || decl.id != entry.variant {
                continue;
            }
            let download = if entry.mode == ModelMode::Reference {
                Some(reference_descriptor(mf, decl)?)
            } else {
                None
            };
            return Ok(ResolvedModel {
                module_id: mf.module.id.clone(),
                model_id: decl.id.clone(),
                target_dir: decl.target_dir.clone(),
                backends: mf.compute.backends.clone(),
                download,
            });
        }
    }
    Err(format!(
        "no installed module provides model {}@{}",
        entry.qualified_id, entry.variant
    ))
}

/// reference 下载描述符解析（缺 repo_id/url → Err → 适配判 Unsupported）
fn reference_descriptor(mf: &ModuleManifest, decl: &ModelDecl) -> Result<PendingDownload, String> {
    match decl.source {
        ModelSource::Huggingface | ModelSource::Modelscope => {
            let location = decl.repo_id.clone().ok_or_else(|| {
                format!(
                    "module '{}' model '{}' declares {} source without repo_id",
                    mf.module.id,
                    decl.id,
                    decl.source
                )
            })?;
            Ok(PendingDownload {
                source: decl.source.as_str().to_string(),
                location,
                revision: decl.revision.clone(),
            })
        }
        ModelSource::Url => {
            let location = decl.url.clone().ok_or_else(|| {
                format!(
                    "module '{}' model '{}' declares url source without url",
                    mf.module.id, decl.id
                )
            })?;
            Ok(PendingDownload {
                source: decl.source.as_str().to_string(),
                location,
                revision: decl.revision.clone(),
            })
        }
        ModelSource::LocalImport => {
            // 本地自建（E7）：打包面按引用声明收录 target_dir，不做下载
            Ok(PendingDownload {
                source: decl.source.as_str().to_string(),
                location: decl.target_dir.clone(),
                revision: decl.revision.clone(),
            })
        }
    }
}

/// GET /api/packs/{id} 的实时适配报告：库层条目（B1 `adapt_model`）→
/// S2 前端形状（仲裁 #3/#4）。pack 后端信息未随注册表持久化，
/// 以模块声明后端为有效集上界（本机视角的「将运行于」实时结论）。
fn live_adaptation(
    devices: &[ComputeDevice],
    manifests: &[ModuleManifest],
    models: &[ep_pack::import::InstalledPackModel],
    lang: &str,
) -> Vec<AdaptationOut> {
    models
        .iter()
        .map(|m| {
            let entry = PackModelEntry {
                qualified_id: m.qualified_id.clone(),
                variant: m.variant.clone(),
                mode: m.mode,
                tags: m.tags.clone(),
            };
            let resolved = resolve_entry(manifests, &entry);
            let pack_backends: Vec<ComputeBackend> = resolved
                .as_ref()
                .map(|r| r.backends.clone())
                .unwrap_or_default();
            let result = adapt_model(&entry, &resolved, &pack_backends, devices);

            let (ok, note) = match result.verdict {
                AdaptationVerdict::Device => {
                    let device = result.device.clone().unwrap_or_default();
                    (
                        true,
                        ep_core::i18n::t(
                            lang,
                            "packs:adaptDevice",
                            &[("device", device.as_str())],
                        ),
                    )
                }
                AdaptationVerdict::CpuFallback => {
                    (true, ep_core::i18n::t(lang, "packs:adaptCpuFallback", &[]))
                }
                AdaptationVerdict::Unsupported => (
                    false,
                    ep_core::i18n::t(
                        lang,
                        "packs:adaptUnsupported",
                        &[("reason", result.reason.as_str())],
                    ),
                ),
            };
            AdaptationOut {
                qualified_id: m.qualified_id.clone(),
                variant: m.variant.clone(),
                ok,
                device: result.device.clone(),
                note,
            }
        })
        .collect()
}

// ─── 导入后台任务（委托 B1 编排核心）───────────────────────────────────────

/// 启动导入后台任务（handler 层已完成来源解析与 pack_id 快读）。
///
/// `remove_archive`：URL 下载 / 浏览器上传的临时归档在导入结束后删除；
/// 本地路径导入不删用户文件。
fn spawn_import_task(state: Arc<AppState>, archive: PathBuf, remove_archive: bool, pack_id: String) {
    info!(pack_id = %pack_id, "API: pack import accepted");
    // 受理即广播一条初始进度，方便刚错过 202 的客户端同步状态
    let _ = state.model_download_tx.send(WsMessage::PackImport {
        pack_id: pack_id.clone(),
        stage: Some("accepted".to_string()),
        percent: Some(0.0),
        state: Some("running".to_string()),
        message: None,
    });
    tokio::spawn(async move {
        let tx = state.model_download_tx.clone();
        let task_pack_id = pack_id.clone();
        let blocking_pack_id = pack_id.clone();
        let result = tokio::task::spawn_blocking(move || {
            run_import(state, archive, remove_archive, &blocking_pack_id)
        })
        .await;
        if let Err(join) = result {
            let _ = tx.send(WsMessage::PackImport {
                pack_id: task_pack_id,
                stage: None,
                percent: None,
                state: Some("failed".to_string()),
                message: Some(format!("import task panicked: {join}")),
            });
        }
    });
}

/// §4.4 导入全流程（阻塞执行于 spawn_blocking）：委托
/// [`ep_pack::import::import_pack`]，成功后驱动 reference 模型后台下载。
/// 终态 WS 消息恒在本函数内发出（completed/failed）。
fn run_import(state: Arc<AppState>, archive: PathBuf, remove_archive: bool, pack_id: &str) {
    let lang = {
        let raw = state.config.blocking_read().general.language.clone();
        ep_core::i18n::normalize_language(&raw).to_string()
    };
    let tx = state.model_download_tx.clone();

    // 落位目标与选项（models_dir 按配置解析覆盖 ImportTargets 默认值）
    let (staging_cfg, models_dir) = {
        let cfg = state.config.blocking_read();
        (
            cfg.packs.staging_dir.clone(),
            cfg.resolve_model_cache_dir(&state.root),
        )
    };
    let staging = staging_root(&state.root, &staging_cfg);
    let _ = std::fs::create_dir_all(&staging);
    let mut targets = ImportTargets::from_root(&state.root);
    targets.models_dir = models_dir;
    let options = ImportOptions {
        current_ep_version: EP_VERSION.to_string(),
        ..Default::default()
    };

    let devices = state.devices.blocking_read().clone();
    let manifests = state
        .modules
        .blocking_read()
        .iter()
        .filter_map(|m| m.manifest.clone())
        .collect::<Vec<_>>();

    // 进度回调 → WS pack_import（stage 直传 B1 小写阶段名，state 包络 running）
    let pid = pack_id.to_string();
    let progress_tx = tx.clone();
    let progress = move |p: PackImportProgress| {
        let _ = progress_tx.send(WsMessage::PackImport {
            pack_id: pid.clone(),
            stage: Some(p.stage.as_str().to_string()),
            percent: Some(p.percent as f32),
            state: Some("running".to_string()),
            message: Some(p.message),
        });
    };

    let result = run_import_core(
        &archive,
        &staging,
        &targets,
        &options,
        &devices,
        |entry| resolve_entry(&manifests, entry),
        progress,
    );

    match result {
        Ok(report) => {
            if !report.warnings.is_empty() {
                warn!(pack_id = %pack_id, warnings = ?report.warnings, "pack import warnings");
            }
            if !report.pipeline_conflicts.is_empty() {
                // 冲突不阻断导入（B1 语义）；覆盖/改名决策待 API/UI 层后续迭代
                info!(
                    pack_id = %pack_id,
                    conflicts = ?report.pipeline_conflicts.iter().map(|c| c.pipeline_id.clone()).collect::<Vec<_>>(),
                    "pipeline conflicts skipped during import"
                );
            }
            let models_count = report.installed_models.len().to_string();
            let downloads_count = report.pending_downloads.len().to_string();
            let pipelines_count = report.pipelines_installed.len().to_string();

            // reference 模型 → 后台下载（复用 DownloadHandle 进度设施）。
            // 附 meta 补丁上下文（仲裁返工）：下载终态 completed 后回填
            // pack_id/qualified_id/tags，使 DELETE 的 meta.pack_id 扫描覆盖
            // reference 后下载模型。tags 取条目 tags ∪ 包级 tags（B1
            // merge_tags 同款语义），故需重读一次归档清单；读失败仅降级为
            // 空 tags（pack_id/qualified_id 补丁不受影响）。
            let tag_manifest = match read_manifest_from_archive(&archive) {
                Ok(m) => Some(m),
                Err(e) => {
                    warn!(
                        pack_id = %pack_id,
                        error_key = e.key,
                        "re-read manifest for reference meta tags failed; patch tags empty"
                    );
                    None
                }
            };
            let patches: Vec<RefMetaPatch> = report
                .pending_downloads
                .iter()
                .map(|req| ref_meta_patch(tag_manifest.as_ref(), pack_id, req))
                .collect();
            start_pending_downloads(&state, report.pending_downloads, patches);

            let message = ep_core::i18n::t(
                &lang,
                "packs:importDone",
                &[
                    ("models", models_count.as_str()),
                    ("downloads", downloads_count.as_str()),
                    ("pipelines", pipelines_count.as_str()),
                ],
            );
            let _ = tx.send(WsMessage::PackImport {
                pack_id: pack_id.to_string(),
                stage: Some("done".to_string()),
                percent: Some(100.0),
                state: Some("completed".to_string()),
                message: Some(message),
            });
            info!(pack_id = %pack_id, "pack import completed");
        }
        Err(e) => {
            let fail = import_failure(&e);
            let params: Vec<(&str, &str)> = fail
                .params
                .iter()
                .map(|(k, v)| (*k, v.as_str()))
                .collect();
            let message = ep_core::i18n::t(&lang, fail.key, &params);
            warn!(pack_id = %pack_id, error = %e, "pack import failed");
            let _ = tx.send(WsMessage::PackImport {
                pack_id: pack_id.to_string(),
                stage: None,
                percent: None,
                state: Some("failed".to_string()),
                message: Some(message),
            });
        }
    }

    if remove_archive {
        let _ = std::fs::remove_file(&archive);
    }
}

// ─── reference 模型后台下载驱动（复用 DownloadHandle 进度设施，§4.4）──────

/// reference 模型下载完成后的 meta 补丁上下文（仲裁返工）：
/// 下载终态 completed 后回填 `pack_id`/`qualified_id`/合并 tags，
/// 使 `DELETE /api/packs/{id}` 按 `meta.pack_id` 的扫描能覆盖
/// reference 后下载模型（否则仅 bundle 落位模型可被卸载删除）。
#[derive(Debug, Clone)]
struct RefMetaPatch {
    pack_id: String,
    qualified_id: String,
    /// 条目 tags ∪ 包级 tags，去重保序（B1 merge_tags 同款语义）
    tags: Vec<String>,
}

/// 合并 tags（B1 `merge_tags` 同款）：条目 tags 在前、包级 tags 在后，去重保序。
fn merge_pack_tags(entry_tags: &[String], pack_tags: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for tag in entry_tags.iter().chain(pack_tags.iter()) {
        if !out.contains(tag) {
            out.push(tag.clone());
        }
    }
    out
}

/// 由待下载请求 + 归档清单构造 meta 补丁上下文。
/// 清单缺失（理论不可达：导入刚从同一归档成功）→ tags 降级为空。
fn ref_meta_patch(
    manifest: Option<&PackManifest>,
    pack_id: &str,
    req: &PendingDownloadRequest,
) -> RefMetaPatch {
    let (entry_tags, pack_tags) = match manifest {
        Some(m) => {
            let entry_tags = m
                .models
                .iter()
                .find(|e| e.qualified_id == req.qualified_id && e.variant == req.variant)
                .map(|e| e.tags.clone())
                .unwrap_or_default();
            (entry_tags, m.pack.tags.clone())
        }
        None => (Vec::new(), Vec::new()),
    };
    RefMetaPatch {
        pack_id: pack_id.to_string(),
        qualified_id: req.qualified_id.clone(),
        tags: merge_pack_tags(&entry_tags, &pack_tags),
    }
}

/// 逐条启动 reference 模型下载（每条独立 async 任务；失败仅 warn 不阻断）
fn start_pending_downloads(
    state: &Arc<AppState>,
    pending: Vec<PendingDownloadRequest>,
    patches: Vec<RefMetaPatch>,
) {
    for (req, patch) in pending.into_iter().zip(patches) {
        let st = state.clone();
        tokio::spawn(async move {
            start_one_pending_download(st, req, patch).await;
        });
    }
}

/// 单个 reference 模型下载：manifest 查找 → venv 准备 → 启动 → 监督中继。
/// 与 api/models.rs 下载端点同语义（downloads 表 + WS model_download）。
async fn start_one_pending_download(
    state: Arc<AppState>,
    req: PendingDownloadRequest,
    patch: RefMetaPatch,
) {
    // 1. 模块清单与模型声明
    let modules = state.modules.read().await;
    let manifest = modules
        .iter()
        .find(|m| {
            m.manifest
                .as_ref()
                .map(|mf| mf.module.id == req.module_id)
                .unwrap_or(false)
        })
        .and_then(|m| m.manifest.clone());
    drop(modules);
    let Some(manifest) = manifest else {
        warn!(module_id = %req.module_id, "pending download: module not found, skipped");
        return;
    };
    let Some(decl) = manifest
        .models
        .iter()
        .find(|d| d.id == req.model_id)
        .cloned()
    else {
        warn!(
            module_id = %req.module_id,
            model_id = %req.model_id,
            "pending download: model decl not found, skipped"
        );
        return;
    };

    let mgr = build_model_manager(&state).await;
    if mgr.is_model_present(&req.target_dir) {
        debug!(target_dir = %req.target_dir, "pending download: model already present");
        return;
    }
    let key = format!("{}:{}", req.module_id, req.model_id);
    {
        let map = state.downloads.lock().unwrap_or_else(|e| e.into_inner());
        if map.get(&key).is_some_and(|e| e.state == "downloading") {
            return; // 已在下载
        }
    }

    // 2. venv 就绪门禁（任务 #10：与 models.rs / modules.rs / autostart.rs 同源
    //    的共享助手，哈希门禁修复"半壳 venv"误判；失败仅告警并跳过本次下载）
    let venv_python = match super::ensure_module_venv_ready(&state, &req.module_id, &manifest, manifest.compute.default_backend)
        .await
    {
        Ok(path) => path,
        Err(e) => {
            warn!(module_id = %req.module_id, error = %e, "pending download: venv prep failed");
            return;
        }
    };

    // 3. 启动下载（ep-core spawn python 子进程 + 进度广播）
    let config = state.config.read().await.clone();
    let handle = match mgr.execute_download_with_progress(
        &req.module_id,
        &decl,
        &venv_python,
        &config,
        None,
    ) {
        Ok(h) => h,
        Err(e) => {
            warn!(
                module_id = %req.module_id,
                model_id = %req.model_id,
                error = ?e,
                "pending download: failed to start"
            );
            return;
        }
    };

    // 4. downloads 表登记 + 初始 WS + 监督任务
    {
        let mut map = state.downloads.lock().unwrap_or_else(|e| e.into_inner());
        map.insert(
            key.clone(),
            DownloadEntry {
                module_id: req.module_id.clone(),
                model_id: req.model_id.clone(),
                source: req.download.source.clone(),
                percent: 0.0,
                bytes: 0,
                state: "downloading".to_string(),
                started_at: chrono::Utc::now().to_rfc3339(),
            },
        );
    }
    let _ = state.model_download_tx.send(WsMessage::ModelDownload {
        module_id: req.module_id.clone(),
        model_id: req.model_id.clone(),
        percent: 0.0,
        state: "downloading".to_string(),
        bytes: 0,
    });
    info!(
        module_id = %req.module_id,
        model_id = %req.model_id,
        pack_source = %req.download.source,
        "pack reference model download started"
    );

    let downloads = state.downloads.clone();
    let ws_tx = state.model_download_tx.clone();
    let target_dir = req.target_dir.clone();
    tokio::spawn(async move {
        monitor_pack_download(handle, downloads, ws_tx, key, state, target_dir, patch).await;
    });
}

/// 中继下载进度直到结束（模式同 models.rs monitor_download 的精简版）：
/// 每条进度更新 downloads 表并广播 WS model_download；结束落终态。
/// 终态 completed 后追加 reference meta 补丁（仲裁返工）。
async fn monitor_pack_download(
    handle: DownloadHandle,
    downloads: Arc<std::sync::Mutex<std::collections::HashMap<String, DownloadEntry>>>,
    ws_tx: tokio::sync::broadcast::Sender<WsMessage>,
    key: String,
    state: Arc<AppState>,
    target_dir: String,
    patch: RefMetaPatch,
) {
    let module_id = handle.module_id().to_string();
    let model_id = handle.model_id().to_string();
    let mut rx = handle.subscribe_progress();
    let wait_fut = handle.wait();
    tokio::pin!(wait_fut);

    let mut final_state: Option<String> = None;
    let mut final_percent: f32 = 0.0;
    let mut final_bytes: u64 = 0;

    loop {
        tokio::select! {
            recv = rx.recv() => match recv {
                Ok(p) => {
                    relay_pack_download(&downloads, &ws_tx, &key, &p);
                    final_percent = p.percent;
                    final_bytes = p.bytes;
                    final_state = Some(download_state_string(&p.state));
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    debug!(key = %key, lagged = n, "pack download progress lagged");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            },
            res = &mut wait_fut => {
                // 抽干队列中剩余事件（状态终值以 wait 结果为准，此处仅同步 percent/bytes）
                while let Ok(p) = rx.try_recv() {
                    relay_pack_download(&downloads, &ws_tx, &key, &p);
                    final_percent = p.percent;
                    final_bytes = p.bytes;
                }
                final_state = Some(match res {
                    Ok(_) => "completed".to_string(),
                    Err(e) => {
                        warn!(key = %key, error = %e, "pack reference download failed");
                        "failed".to_string()
                    }
                });
                if final_state.as_deref() == Some("completed") {
                    final_percent = 100.0;
                }
                break;
            }
        }
    }

    let state_str = final_state.unwrap_or_else(|| "failed".to_string());
    {
        let mut map = downloads.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = map.get_mut(&key) {
            entry.state = state_str.clone();
            entry.percent = final_percent;
            entry.bytes = final_bytes;
        }
    }
    let _ = ws_tx.send(WsMessage::ModelDownload {
        module_id,
        model_id,
        percent: final_percent,
        state: state_str.clone(),
        bytes: final_bytes,
    });

    // 仲裁返工：下载终态 completed 后回填 meta（pack_id/qualified_id/合并 tags）。
    // 时序安全：ep-core 监督任务在发送 done(Ok) 前已写入 .ep_meta.json。
    // best-effort：失败仅 warn，绝不影响导入成功语义。
    if state_str == "completed" {
        patch_reference_meta(&state, &target_dir, &patch).await;
    }
}

/// reference 模型下载完成后的 meta 补丁（仲裁返工）：读现有 `.ep_meta.json`
/// → 设置 `pack_id`/`qualified_id`、覆盖为合并 tags（条目 ∪ 包级，B1
/// merge_tags 同款）→ 写回。使 `DELETE /api/packs/{id}` 按 `meta.pack_id`
/// 的扫描能覆盖 reference 后下载模型。
///
/// best-effort：meta 不存在 / 写回失败均仅 warn，不向上传播（下载本身已成功，
/// 导入语义不受影响）。
async fn patch_reference_meta(state: &Arc<AppState>, target_dir: &str, patch: &RefMetaPatch) {
    let mgr = build_model_manager(state).await;
    let Some(mut meta) = mgr.read_meta(target_dir) else {
        warn!(
            target_dir = %target_dir,
            pack_id = %patch.pack_id,
            "reference meta patch skipped: meta not found (non-fatal)"
        );
        return;
    };
    meta.pack_id = Some(patch.pack_id.clone());
    meta.qualified_id = Some(patch.qualified_id.clone());
    meta.tags = patch.tags.clone();
    if let Err(e) = mgr.write_meta(target_dir, &meta) {
        warn!(
            target_dir = %target_dir,
            error = %e,
            "reference meta patch write failed (non-fatal)"
        );
    } else {
        info!(
            target_dir = %target_dir,
            pack_id = %patch.pack_id,
            qualified_id = %patch.qualified_id,
            "reference model meta patched with pack identity"
        );
    }
}

fn relay_pack_download(
    downloads: &std::sync::Mutex<std::collections::HashMap<String, DownloadEntry>>,
    ws_tx: &tokio::sync::broadcast::Sender<WsMessage>,
    key: &str,
    p: &DownloadProgress,
) {
    let state_str = download_state_string(&p.state);
    {
        let mut map = downloads.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = map.get_mut(key) {
            entry.percent = p.percent;
            entry.bytes = p.bytes;
            entry.state = state_str.clone();
        }
    }
    let _ = ws_tx.send(WsMessage::ModelDownload {
        module_id: p.module_id.clone(),
        model_id: p.model_id.clone(),
        percent: p.percent,
        state: state_str,
        bytes: p.bytes,
    });
}

fn download_state_string(state: &DownloadState) -> String {
    match state {
        DownloadState::Downloading => "downloading".to_string(),
        DownloadState::Completed => "completed".to_string(),
        DownloadState::Failed(_) => "failed".to_string(),
        DownloadState::Cancelled => "cancelled".to_string(),
    }
}

// ─── 构建 / 导出（§4.5）─────────────────────────────────────────────────────

/// 后台构建任务描述（handler 层完成圈选与校验后移交给 spawn_blocking）
struct BuildJob {
    pack_id: String,
    manifest: PackManifest,
    /// bundle 权重：归档相对 target_dir → 磁盘源目录
    bundle_dirs: Vec<(String, PathBuf)>,
    /// 管线文件：磁盘源文件 → 归档内文件名（pipelines/ 下）
    pipeline_files: Vec<(PathBuf, String)>,
}

/// handler 层同步圈选 + 校验 → [`BuildJob`]（快速失败在 202 之前）。
///
/// 圈选匹配基于 meta/decl 的 qualified_id（§4.3）；tags 路径匹配
/// meta.tags（§4.5 tag 组装闭环）。qualified_id 缺失的已装模型无法
/// 被 models[] 圈选（身份缺失），tags 圈选同样跳过。
async fn prepare_build_job(
    state: &Arc<AppState>,
    req: BuildRequest,
) -> Result<BuildJob, PackApiError> {
    if req.models.is_empty() && req.tags.is_empty() && req.pipelines.is_empty() {
        return Err(PackApiError::detail(
            StatusCode::BAD_REQUEST,
            "packs:errorBuildInvalid",
            "at least one of models / tags / pipelines must be non-empty",
        ));
    }

    // pin / bundle 语法校验
    let mut pins = Vec::with_capacity(req.models.len());
    for raw in &req.models {
        let pin = PinnedModelId::parse(raw).map_err(|e| {
            PackApiError::detail(StatusCode::BAD_REQUEST, "packs:errorBuildInvalid", e)
        })?;
        pins.push(pin);
    }
    let mut bundle_set = Vec::with_capacity(req.bundle.len());
    for raw in &req.bundle {
        let qid = QualifiedId::parse(raw).map_err(|e| {
            PackApiError::detail(StatusCode::BAD_REQUEST, "packs:errorBuildInvalid", e)
        })?;
        bundle_set.push(qid.to_canonical());
    }

    let manifests = read_manifests(state).await;
    let mgr = build_model_manager(state).await;
    let installed = mgr.list_downloaded_models();

    // 圈选：pins（meta.qualified_id + variant 匹配）∪ tags（meta.tags 交集）
    // selected: (qualified_id, variant, tags, target_dir)
    let mut selected: Vec<(String, String, Vec<String>, String)> = Vec::new();
    let mut unmatched: Vec<String> = Vec::new();
    for pin in &pins {
        let qid = pin.id.to_canonical();
        let hit = installed.iter().find(|m| {
            m.meta.qualified_id.as_deref() == Some(qid.as_str())
                && pin
                    .variant
                    .as_ref()
                    .map(|v| *v == m.meta.model_id)
                    .unwrap_or(true)
        });
        match hit {
            Some(m) => selected.push((
                qid,
                m.meta.model_id.clone(),
                m.meta.tags.clone(),
                m.target_dir.clone(),
            )),
            None => unmatched.push(pin.to_canonical()),
        }
    }
    if !req.tags.is_empty() {
        for m in &installed {
            let tagged = m.meta.tags.iter().any(|t| req.tags.contains(t));
            if !tagged {
                continue;
            }
            let Some(qid) = m.meta.qualified_id.clone() else {
                continue; // 无 qualified_id 的模型无法入包（§4.3 身份缺失）
            };
            if !selected
                .iter()
                .any(|(q, v, _, _)| *q == qid && *v == m.meta.model_id)
            {
                selected.push((
                    qid,
                    m.meta.model_id.clone(),
                    m.meta.tags.clone(),
                    m.target_dir.clone(),
                ));
            }
        }
    }
    if !unmatched.is_empty() {
        return Err(PackApiError::with_params(
            StatusCode::BAD_REQUEST,
            "packs:errorBuildNoModels",
            vec![("detail", unmatched.join(", "))],
        ));
    }
    if selected.is_empty() && req.pipelines.is_empty() {
        return Err(PackApiError::detail(
            StatusCode::BAD_REQUEST,
            "packs:errorBuildNoModels",
            "selection matched no installed models with qualified_id",
        ));
    }

    // 模型条目 + bundle 权重目录 + 后端并集
    let mut entries: Vec<PackModelEntry> = Vec::new();
    let mut bundle_dirs: Vec<(String, PathBuf)> = Vec::new();
    let mut backends: Vec<ComputeBackend> = Vec::new();
    for (qid, variant, tags, target_dir) in &selected {
        let bundle = bundle_set.contains(qid);
        if bundle {
            let src = mgr.model_dir(target_dir);
            if !src.is_dir() {
                return Err(PackApiError::with_params(
                    StatusCode::BAD_REQUEST,
                    "packs:errorBuildInvalid",
                    vec![(
                        "detail",
                        format!("bundle model {qid}@{variant}: dir {} missing", src.display()),
                    )],
                ));
            }
            bundle_dirs.push((target_dir.clone(), src));
        }
        if let Some((mf, _)) = resolve_decl(&manifests, qid, variant) {
            for b in &mf.compute.backends {
                if !backends.contains(b) {
                    backends.push(*b);
                }
            }
        }
        entries.push(PackModelEntry {
            qualified_id: qid.clone(),
            variant: variant.clone(),
            mode: if bundle {
                ModelMode::Bundle
            } else {
                ModelMode::Reference
            },
            tags: tags.clone(),
        });
    }
    if backends.is_empty() {
        backends.push(ComputeBackend::Cpu);
    }

    // 管线圈选：按 id 查找 config/pipelines/*.toml
    let pipelines_dir = state.root.join("config").join("pipelines");
    let specs = scan_pipeline_specs(&pipelines_dir);
    let mut pipeline_files: Vec<(PathBuf, String)> = Vec::new();
    let mut pipeline_refs: Vec<PackPipelineRef> = Vec::new();
    let mut missing_pipelines: Vec<String> = Vec::new();
    for pid in &req.pipelines {
        match specs.iter().find(|(_, spec)| spec.pipeline.id == *pid) {
            Some((path, _)) => {
                pipeline_files.push((path.clone(), format!("{pid}.toml")));
                pipeline_refs.push(PackPipelineRef {
                    file: format!("pipelines/{pid}.toml"),
                });
            }
            None => missing_pipelines.push(pid.clone()),
        }
    }
    if !missing_pipelines.is_empty() {
        return Err(PackApiError::with_params(
            StatusCode::BAD_REQUEST,
            "packs:errorBuildInvalid",
            vec![(
                "detail",
                format!("pipeline(s) not found: {}", missing_pipelines.join(", ")),
            )],
        ));
    }

    // 包身份：优先请求显式字段，缺省自动生成
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let id = req.id.unwrap_or_else(|| format!("local.build-{stamp}"));
    let version = req.version.unwrap_or_else(|| "0.1.0".to_string());
    let name = req.name.unwrap_or_else(|| id.clone());
    let description = req.description.unwrap_or_default();

    let manifest = PackManifest {
        pack: ep_pack::manifest::PackInfo {
            id: id.clone(),
            version,
            name,
            description,
            authors: vec![],
            license: None,
            homepage: None,
            min_ep_version: None,
            tags: req.tags.clone(),
        },
        compute: ep_pack::manifest::PackCompute {
            backends,
            notes: std::collections::HashMap::new(),
        },
        models: entries,
        pipelines: pipeline_refs,
    };
    if let Err(errors) = manifest.validate() {
        return Err(PackApiError::with_params(
            StatusCode::BAD_REQUEST,
            "packs:errorBuildInvalid",
            vec![("detail", errors.join("; "))],
        ));
    }

    Ok(BuildJob {
        pack_id: id,
        manifest,
        bundle_dirs,
        pipeline_files,
    })
}

/// 按 qualified_id(+variant) 在模块清单中解析声明（构建/导出重建共用）
fn resolve_decl<'a>(
    manifests: &'a [ModuleManifest],
    qualified_id: &str,
    variant: &str,
) -> Option<(&'a ModuleManifest, &'a ModelDecl)> {
    for mf in manifests {
        for decl in &mf.models {
            let Some(q) = decl.qualified_id.as_deref() else {
                continue;
            };
            let Ok(parsed) = QualifiedId::parse(q) else {
                continue;
            };
            if parsed.to_canonical() == qualified_id && decl.id == variant {
                return Some((mf, decl));
            }
        }
    }
    None
}

/// 扫描目录下管线 spec（损坏文件跳过；模式同 api/pipelines.rs scan_specs）
fn scan_pipeline_specs(dir: &Path) -> Vec<(PathBuf, pipeline_bridge::PipelineSpec)> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    let mut paths: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("toml"))
        .collect();
    paths.sort();
    for path in paths {
        match pipeline_bridge::load_spec(&path) {
            Ok(spec) => out.push((path, spec)),
            Err(e) => {
                warn!(file = %path.display(), error = %e, "pipeline file corrupted, skipping");
            }
        }
    }
    out
}

fn spawn_build_task(state: Arc<AppState>, job: BuildJob) {
    info!(pack_id = %job.pack_id, "API: pack build accepted");
    tokio::spawn(async move {
        let tx = state.model_download_tx.clone();
        let pack_id = job.pack_id.clone();
        let result = tokio::task::spawn_blocking(move || run_build(state, job)).await;
        let failure_message = match result {
            Ok(Ok(())) => None,
            Ok(Err(message)) => Some(message),
            Err(join) => Some(format!("build task panicked: {join}")),
        };
        if let Some(message) = failure_message {
            let _ = tx.send(WsMessage::PackImport {
                pack_id,
                stage: Some("build".to_string()),
                percent: None,
                state: Some("failed".to_string()),
                message: Some(message),
            });
        }
    });
}

/// 后台构建：组装包内容目录 → [`ep_pack::build::build_pack`] → 产物缓存
/// runtime/pack-out/。完成/失败经 WS pack_import（stage="build"）广播。
fn run_build(state: Arc<AppState>, job: BuildJob) -> Result<(), String> {
    let lang = {
        let raw = state.config.blocking_read().general.language.clone();
        ep_core::i18n::normalize_language(&raw).to_string()
    };
    let staging_cfg = state.config.blocking_read().packs.staging_dir.clone();
    let staging = staging_root(&state.root, &staging_cfg);
    let source_dir = staging.join(format!("build-{}-{}", job.pack_id, unique_id()));
    let output = pack_out_dir(&state.root).join(format!(
        "{}-{}.zip",
        job.manifest.pack.id, job.manifest.pack.version
    ));

    let result = assemble_and_build(&source_dir, &output, &job);
    let _ = std::fs::remove_dir_all(&source_dir); // 无论成败清理暂存

    match result {
        Ok(summary_files) => {
            let files = summary_files.to_string();
            let message = ep_core::i18n::t(&lang, "packs:buildDone", &[("files", files.as_str())]);
            let _ = state.model_download_tx.send(WsMessage::PackImport {
                pack_id: job.pack_id.clone(),
                stage: Some("build".to_string()),
                percent: Some(100.0),
                state: Some("completed".to_string()),
                message: Some(message),
            });
            info!(pack_id = %job.pack_id, archive = %output.display(), "pack build completed");
            Ok(())
        }
        Err(e) => {
            let params: Vec<(&str, &str)> = e
                .params
                .iter()
                .map(|(k, v)| (*k, v.as_str()))
                .collect();
            Err(ep_core::i18n::t(&lang, e.key, &params))
        }
    }
}

/// 组装包内容目录并打包（build 与 export 重建共用）。
/// 返回归档文件条目数（供完成消息）。
fn assemble_and_build(
    source_dir: &Path,
    output: &Path,
    job: &BuildJob,
) -> Result<usize, PackTaskError> {
    std::fs::create_dir_all(source_dir)
        .map_err(|e| PackTaskError::detail("packs:errorInternal", e))?;

    // 1) 清单（ep-pack.toml）：ep-daemon 无 toml 依赖，用内置最小 TOML 输出器
    let manifest_toml = render_pack_manifest(&job.manifest);
    std::fs::write(
        source_dir.join(ep_pack::extract::MANIFEST_FILE_NAME),
        manifest_toml,
    )
    .map_err(|e| PackTaskError::detail("packs:errorInternal", e))?;

    // 2) bundle 权重：models/<target_dir>/
    for (target_dir, src) in &job.bundle_dirs {
        let dest = source_dir.join("models").join(target_dir);
        std::fs::create_dir_all(&dest)
            .map_err(|e| PackTaskError::detail("packs:errorInternal", e))?;
        copy_dir_contents(src, &dest)
            .map_err(|e| PackTaskError::detail("packs:errorInternal", e))?;
    }

    // 3) 管线文件：pipelines/<id>.toml
    for (src, file_name) in &job.pipeline_files {
        let dest = source_dir.join("pipelines").join(file_name);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| PackTaskError::detail("packs:errorInternal", e))?;
        }
        std::fs::copy(src, &dest)
            .map_err(|e| PackTaskError::detail("packs:errorInternal", e))?;
    }

    // 4) 打包（CHECKSUMS.toml 由 build_pack 生成并写入归档）
    let plan = BuildPlan::new(source_dir, output);
    let summary = build_pack_archive(&plan).map_err(|e| build_failure(&e))?;
    Ok(summary.file_count)
}

fn build_failure(e: &BuildError) -> PackTaskError {
    match e {
        BuildError::SourceDirMissing { path } => PackTaskError::with_params(
            "packs:errorBuildSourceMissing",
            vec![("path", path.display().to_string())],
        ),
        BuildError::ManifestMissing { path } => PackTaskError::with_params(
            "packs:errorBuildManifestMissing",
            vec![("path", path.display().to_string())],
        ),
        BuildError::OutputInsideSource { .. } => {
            PackTaskError::new("packs:errorBuildOutputInsideSource")
        }
        other => PackTaskError::detail("packs:errorInternal", other),
    }
}

/// 递归复制目录内容（同步；跳过符号链接等非文件类型）
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
    }
    Ok(())
}

/// export 产物定位：缓存命中 → 文件名；已装包 → 即时重建；否则 404。
async fn ensure_export_artifact(state: &Arc<AppState>, id: &str) -> Result<String, PackApiError> {
    // 1) 缓存命中（<id>-*.zip）
    if let Some(name) = find_cached_artifact(&state.root, id) {
        return Ok(name);
    }

    // 2) 已装包：按注册条目重建
    let reg_path = registry_entry_path(&registry_dir(&state.root), id);
    let installed = match read_installed_pack(&reg_path) {
        Ok(Some(p)) => p,
        Ok(None) => {
            return Err(PackApiError::with_params(
                StatusCode::NOT_FOUND,
                "packs:errorExportNotBuilt",
                vec![("id", id.to_string())],
            ))
        }
        Err(e) => {
            return Err(PackApiError::detail(
                StatusCode::INTERNAL_SERVER_ERROR,
                "packs:errorInternal",
                e,
            ))
        }
    };

    let job = rebuild_job_from_entry(state, &installed).await?;
    let job_pack_id = job.pack_id.clone();
    let build_state = state.clone();
    let build =
        tokio::task::spawn_blocking(move || run_build_for_export(build_state, job)).await;
    match build {
        Ok(Ok(())) => find_cached_artifact(&state.root, &job_pack_id).ok_or_else(|| {
            PackApiError::with_params(
                StatusCode::INTERNAL_SERVER_ERROR,
                "packs:errorInternal",
                vec![(
                    "detail",
                    "rebuild succeeded but artifact not found".to_string(),
                )],
            )
        }),
        Ok(Err(e)) => Err(PackApiError::with_params(
            StatusCode::INTERNAL_SERVER_ERROR,
            "packs:errorInternal",
            vec![("detail", e)],
        )),
        Err(join) => Err(PackApiError::detail(
            StatusCode::INTERNAL_SERVER_ERROR,
            "packs:errorInternal",
            join,
        )),
    }
}

/// 查找 pack-out 下 `<id>-*.zip` 缓存产物（文件名，按字典序取最后一个）
fn find_cached_artifact(root: &Path, id: &str) -> Option<String> {
    let dir = pack_out_dir(root);
    let mut matches: Vec<String> = std::fs::read_dir(&dir)
        .ok()?
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with(&format!("{id}-"))
                && (name.ends_with(".zip")
                    || name.ends_with(".tar.gz")
                    || name.ends_with(".tgz")
                    || name.ends_with(".zip"))
            {
                Some(name)
            } else {
                None
            }
        })
        .collect();
    matches.sort();
    matches.pop()
}

/// 由注册条目重建 BuildJob（导出用）：bundle 条目按 qualified_id@variant
/// 反查模块声明定位权重目录（无法定位 → 降级 reference 并 warn）；
/// 管线按已装 id 从 config/pipelines/ 取回。
async fn rebuild_job_from_entry(
    state: &Arc<AppState>,
    entry: &InstalledPack,
) -> Result<BuildJob, PackApiError> {
    let manifests = read_manifests(state).await;
    let mgr = build_model_manager(state).await;

    let mut entries: Vec<PackModelEntry> = Vec::new();
    let mut bundle_dirs: Vec<(String, PathBuf)> = Vec::new();
    for model in &entry.models {
        let mut mode = model.mode;
        if mode == ModelMode::Bundle {
            match resolve_decl(&manifests, &model.qualified_id, &model.variant) {
                Some((_, decl)) => {
                    let src = mgr.model_dir(&decl.target_dir);
                    if src.is_dir() {
                        bundle_dirs.push((decl.target_dir.clone(), src));
                    } else {
                        warn!(
                            pack_id = %entry.id,
                            model = %model.qualified_id,
                            "bundle weights missing on disk; export degrades model to reference"
                        );
                        mode = ModelMode::Reference;
                    }
                }
                None => {
                    warn!(
                        pack_id = %entry.id,
                        model = %model.qualified_id,
                        "owning module not found; export degrades bundle model to reference"
                    );
                    mode = ModelMode::Reference;
                }
            }
        }
        entries.push(PackModelEntry {
            qualified_id: model.qualified_id.clone(),
            variant: model.variant.clone(),
            mode,
            tags: model.tags.clone(),
        });
    }

    let pipelines_dir = state.root.join("config").join("pipelines");
    let specs = scan_pipeline_specs(&pipelines_dir);
    let mut pipeline_files: Vec<(PathBuf, String)> = Vec::new();
    let mut pipeline_refs: Vec<PackPipelineRef> = Vec::new();
    for pid in &entry.pipelines {
        if let Some((path, _)) = specs.iter().find(|(_, spec)| spec.pipeline.id == *pid) {
            pipeline_files.push((path.clone(), format!("{pid}.toml")));
            pipeline_refs.push(PackPipelineRef {
                file: format!("pipelines/{pid}.toml"),
            });
        }
    }

    // 注册表未持久化 [compute].backends：按模型所属模块后端并集重建
    let mut backends: Vec<ComputeBackend> = Vec::new();
    for model in &entries {
        if let Some((mf, _)) = resolve_decl(&manifests, &model.qualified_id, &model.variant) {
            for b in &mf.compute.backends {
                if !backends.contains(b) {
                    backends.push(*b);
                }
            }
        }
    }
    if backends.is_empty() {
        backends.push(ComputeBackend::Cpu);
    }

    let manifest = PackManifest {
        pack: ep_pack::manifest::PackInfo {
            id: entry.id.clone(),
            version: entry.version.clone(),
            name: entry.name.clone().unwrap_or_else(|| entry.id.clone()),
            description: entry.description.clone().unwrap_or_default(),
            authors: vec![],
            license: None,
            homepage: None,
            min_ep_version: None,
            tags: vec![],
        },
        compute: ep_pack::manifest::PackCompute {
            backends,
            notes: std::collections::HashMap::new(),
        },
        models: entries,
        pipelines: pipeline_refs,
    };
    manifest.validate().map_err(|errors| {
        PackApiError::with_params(
            StatusCode::INTERNAL_SERVER_ERROR,
            "packs:errorInternal",
            vec![("detail", errors.join("; "))],
        )
    })?;

    Ok(BuildJob {
        pack_id: entry.id.clone(),
        manifest,
        bundle_dirs,
        pipeline_files,
    })
}

/// 组装 + 打包的无 WS 变体（export 重建用）
fn run_build_for_export(state: Arc<AppState>, job: BuildJob) -> Result<(), String> {
    let staging_cfg = state.config.blocking_read().packs.staging_dir.clone();
    let staging = staging_root(&state.root, &staging_cfg);
    let source_dir = staging.join(format!("rebuild-{}-{}", job.pack_id, unique_id()));
    let output = pack_out_dir(&state.root).join(format!(
        "{}-{}.zip",
        job.manifest.pack.id, job.manifest.pack.version
    ));
    let result = assemble_and_build(&source_dir, &output, &job);
    let _ = std::fs::remove_dir_all(&source_dir);
    match result {
        Ok(_) => Ok(()),
        Err(e) => {
            let params: Vec<(&str, &str)> = e
                .params
                .iter()
                .map(|(k, v)| (*k, v.as_str()))
                .collect();
            Err(ep_core::i18n::t("en", e.key, &params))
        }
    }
}

// ─── 最小 TOML 输出器（ep-pack.toml 生成；ep-daemon 无 toml 依赖）─────────

/// TOML basic string 转义（覆盖清单字段可能出现的引号/反斜杠/控制字符）
fn toml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => {
                out.push_str(&format!("\\u{:04X}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

fn toml_str(s: &str) -> String {
    format!("\"{}\"", toml_escape(s))
}

fn toml_str_array(items: &[String]) -> String {
    let inner: Vec<String> = items.iter().map(|s| toml_str(s)).collect();
    format!("[{}]", inner.join(", "))
}

/// 渲染 [`PackManifest`] 为 TOML 文本（`PackManifest::from_file` 可原样读回，
/// 见单测 roundtrip）。
fn render_pack_manifest(m: &PackManifest) -> String {
    let mut out = String::new();
    out.push_str("[pack]\n");
    out.push_str(&format!("id = {}\n", toml_str(&m.pack.id)));
    out.push_str(&format!("version = {}\n", toml_str(&m.pack.version)));
    out.push_str(&format!("name = {}\n", toml_str(&m.pack.name)));
    out.push_str(&format!(
        "description = {}\n",
        toml_str(&m.pack.description)
    ));
    if !m.pack.authors.is_empty() {
        out.push_str(&format!("authors = {}\n", toml_str_array(&m.pack.authors)));
    }
    if let Some(license) = &m.pack.license {
        out.push_str(&format!("license = {}\n", toml_str(license)));
    }
    if let Some(homepage) = &m.pack.homepage {
        out.push_str(&format!("homepage = {}\n", toml_str(homepage)));
    }
    if let Some(min) = &m.pack.min_ep_version {
        out.push_str(&format!("min_ep_version = {}\n", toml_str(min)));
    }
    if !m.pack.tags.is_empty() {
        out.push_str(&format!("tags = {}\n", toml_str_array(&m.pack.tags)));
    }

    out.push_str("\n[compute]\n");
    let backends: Vec<String> = m.compute.backends.iter().map(|b| b.to_string()).collect();
    out.push_str(&format!("backends = {}\n", toml_str_array(&backends)));
    if !m.compute.notes.is_empty() {
        out.push_str("\n[compute.notes]\n");
        let mut notes: Vec<(String, String)> = m
            .compute
            .notes
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect();
        notes.sort_by(|a, b| a.0.cmp(&b.0));
        for (k, v) in notes {
            out.push_str(&format!("{k} = {}\n", toml_str(&v)));
        }
    }

    for model in &m.models {
        out.push_str("\n[[models]]\n");
        out.push_str(&format!(
            "qualified_id = {}\n",
            toml_str(&model.qualified_id)
        ));
        out.push_str(&format!("variant = {}\n", toml_str(&model.variant)));
        out.push_str(&format!("mode = {}\n", toml_str(model.mode.as_str())));
        if !model.tags.is_empty() {
            out.push_str(&format!("tags = {}\n", toml_str_array(&model.tags)));
        }
    }

    for pipeline in &m.pipelines {
        out.push_str("\n[[pipelines]]\n");
        out.push_str(&format!("file = {}\n", toml_str(&pipeline.file)));
    }
    out
}

// ─── 测试 ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use axum::body::Body;
    use axum::http::{Method, Request};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use ep_core::config::AppConfig;
    use ep_core::port::PortManager;
    use ep_pack::checksum::ChecksumTable;

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    const BOUNDARY: &str = "----ep-packs-test-boundary";

    fn unique_root(tag: &str) -> PathBuf {
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "ep-packs-api-{tag}-{}-{seq}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn test_state(root: PathBuf) -> Arc<AppState> {
        Arc::new(AppState::new(
            root,
            AppConfig::default(),
            vec![],
            vec![],
            PortManager::new(18000, 19000),
        ))
    }

    /// 挂载完整 /api 路由树（同时验证 packs 与其他模块路由无冲突）
    fn app(state: Arc<AppState>) -> Router {
        crate::api::api_router(state.clone()).with_state(state)
    }

    fn get_request(uri: &str) -> Request<Body> {
        Request::builder().uri(uri).body(Body::empty()).unwrap()
    }

    fn delete_request(uri: &str) -> Request<Body> {
        Request::builder()
            .method(Method::DELETE)
            .uri(uri)
            .body(Body::empty())
            .unwrap()
    }

    fn json_request(method: Method, uri: &str, body: Value) -> Request<Body> {
        Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap()
    }

    async fn response_json(resp: axum::response::Response) -> (StatusCode, Value) {
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: Value = serde_json::from_slice(&bytes)
            .unwrap_or_else(|e| panic!("响应不是合法 JSON: {e}; body={bytes:?}"));
        (status, json)
    }

    /// 构造合法最小 .zip（ep-pack.toml + CHECKSUMS.toml）字节
    fn build_test_pack(pack_id: &str) -> Vec<u8> {
        let manifest_toml = format!(
            r#"[pack]
id = "{pack_id}"
version = "1.0.0"
name = "测试包"
description = "roundtrip test pack"

[compute]
backends = ["cpu"]
"#
        );
        // CHECKSUMS.toml：经 ep-pack 生成器对临时目录计算（含 ep-pack.toml 条目）
        let tmp = std::env::temp_dir().join(format!("ep-pack-test-src-{}", unique_id()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join(ep_pack::extract::MANIFEST_FILE_NAME),
            &manifest_toml,
        )
        .unwrap();
        let table = ChecksumTable::generate(&tmp).unwrap();
        let checksums = table.to_toml_string().unwrap();
        std::fs::remove_dir_all(&tmp).ok();

        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        writer
            .start_file(ep_pack::extract::MANIFEST_FILE_NAME, options)
            .unwrap();
        std::io::Write::write_all(&mut writer, manifest_toml.as_bytes()).unwrap();
        writer
            .start_file(ep_pack::checksum::CHECKSUMS_FILE_NAME, options)
            .unwrap();
        std::io::Write::write_all(&mut writer, checksums.as_bytes()).unwrap();
        writer.finish().unwrap().into_inner()
    }

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

    fn multipart_request(body: Vec<u8>) -> Request<Body> {
        Request::builder()
            .method(Method::POST)
            .uri("/packs/upload")
            .header(
                "content-type",
                format!("multipart/form-data; boundary={BOUNDARY}"),
            )
            .header("content-length", body.len().to_string())
            .body(Body::from(body))
            .unwrap()
    }

    /// 手写 B1 注册表条目（InstalledPack 最小形状，供预置已装包场景）
    fn seed_installed_pack(root: &Path, id: &str) {
        let dir = registry_dir(root);
        std::fs::create_dir_all(&dir).unwrap();
        let json = format!(
            r#"{{
  "id": "{id}",
  "version": "1.0.0",
  "name": "预置包",
  "installed_at": "2026-08-05T00:00:00Z",
  "models": [],
  "pipelines": []
}}"#
        );
        std::fs::write(dir.join(format!("{id}.json")), json).unwrap();
    }

    // ── 1. list 空注册表 → 200 + [] ───────────────────────────────────────

    #[tokio::test]
    async fn list_packs_empty_200() {
        let app = app(test_state(unique_root("list")));
        let resp = app.oneshot(get_request("/packs")).await.unwrap();
        let (status, body) = response_json(resp).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!([]));
    }

    // ── 2. import 参数校验 → 4xx ──────────────────────────────────────────

    #[tokio::test]
    async fn import_invalid_body_400() {
        let app = app(test_state(unique_root("import-bad")));

        // 缺 source 判别字段
        let resp = app
            .clone()
            .oneshot(json_request(
                Method::POST,
                "/packs/import",
                json!({ "path": "x.zip" }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        // 未知 source 值
        let resp = app
            .clone()
            .oneshot(json_request(
                Method::POST,
                "/packs/import",
                json!({ "source": "ftp", "url": "ftp://x" }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        // local 缺 path 字段
        let resp = app
            .clone()
            .oneshot(json_request(
                Method::POST,
                "/packs/import",
                json!({ "source": "local" }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        // 非 JSON content-type
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/packs/import")
                    .header("content-type", "text/plain")
                    .body(Body::from("not json"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn import_local_missing_file_400() {
        let root = unique_root("import-missing");
        let app = app(test_state(root.clone()));
        let missing = root.join("no-such-pack.zip");
        let resp = app
            .oneshot(json_request(
                Method::POST,
                "/packs/import",
                json!({ "source": "local", "path": missing.display().to_string() }),
            ))
            .await
            .unwrap();
        let (status, body) = response_json(resp).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body["error"].as_str().is_some());
    }

    #[tokio::test]
    async fn import_url_non_http_400() {
        let app = app(test_state(unique_root("import-url")));
        let resp = app
            .oneshot(json_request(
                Method::POST,
                "/packs/import",
                json!({ "source": "url", "url": "file:///etc/passwd" }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    /// 已装包重复导入 → 409（B1 PackAlreadyInstalled 硬失败的 API 前置拦截）
    #[tokio::test]
    async fn import_already_installed_409() {
        let root = unique_root("import-dup");
        seed_installed_pack(&root, "test.dup-pack");

        // 构造同 id 的 .zip 落盘为本地文件
        let zip_bytes = build_test_pack("test.dup-pack");
        let archive = root.join("dup.zip");
        std::fs::write(&archive, &zip_bytes).unwrap();

        let app = app(test_state(root.clone()));
        let resp = app
            .oneshot(json_request(
                Method::POST,
                "/packs/import",
                json!({ "source": "local", "path": archive.display().to_string() }),
            ))
            .await
            .unwrap();
        let (status, _) = response_json(resp).await;
        assert_eq!(status, StatusCode::CONFLICT);
    }

    // ── 3. upload multipart 形态 ──────────────────────────────────────────

    #[tokio::test]
    async fn upload_missing_file_field_400() {
        let app = app(test_state(unique_root("upload-nofile")));
        let mut body = Vec::new();
        form_part(&mut body, "wrongname", Some("pack.zip"), b"data");
        body.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());

        let resp = app.oneshot(multipart_request(body)).await.unwrap();
        let (status, _) = response_json(resp).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    // ── 4. upload → 202 + 完整导入往返（B1 注册表落条目）─────────────────

    #[tokio::test]
    async fn upload_full_import_roundtrip() {
        let root = unique_root("upload-e2e");
        let state = test_state(root.clone());
        let app = app(state.clone());

        let zip_bytes = build_test_pack("test.upload-pack");
        let mut body = Vec::new();
        form_part(
            &mut body,
            "file",
            Some("test.upload-pack-1.0.0.zip"),
            &zip_bytes,
        );
        body.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());

        let resp = app.clone().oneshot(multipart_request(body)).await.unwrap();
        let (status, json) = response_json(resp).await;
        assert_eq!(status, StatusCode::ACCEPTED, "upload 应 202 受理");
        assert_eq!(json["pack_id"], "test.upload-pack");

        // B1 后台导入完成后注册表文件出现（轮询等待）
        let reg_file = registry_dir(&root).join("test.upload-pack.json");
        let mut found = false;
        for _ in 0..100 {
            if reg_file.exists() {
                found = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(found, "导入完成后 runtime/packs/<id>.json 应落盘");
        let installed: InstalledPack =
            serde_json::from_str(&std::fs::read_to_string(&reg_file).unwrap()).unwrap();
        assert_eq!(installed.version, "1.0.0");
        assert_eq!(installed.name.as_deref(), Some("测试包"));

        // GET /api/packs 列表包含该包（name 物化输出）
        let resp = app.clone().oneshot(get_request("/packs")).await.unwrap();
        let (status, list) = response_json(resp).await;
        assert_eq!(status, StatusCode::OK);
        let arr = list.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["id"], "test.upload-pack");
        assert_eq!(arr[0]["name"], "测试包");
        assert_eq!(arr[0]["version"], "1.0.0");

        // GET /api/packs/{id} 详情含 adaptation 数组（空模型包 → 空数组）
        let resp = app
            .clone()
            .oneshot(get_request("/packs/test.upload-pack"))
            .await
            .unwrap();
        let (status, detail) = response_json(resp).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(detail["id"], "test.upload-pack");
        assert_eq!(detail["adaptation"], json!([]));

        // DELETE → {ok:true}，注册文件移除
        let resp = app
            .clone()
            .oneshot(delete_request("/packs/test.upload-pack"))
            .await
            .unwrap();
        let (status, body) = response_json(resp).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["ok"], true);
        assert!(!reg_file.exists());

        // 再删 → 404
        let resp = app
            .oneshot(delete_request("/packs/test.upload-pack"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    // ── 5. get 未知包 → 404 ───────────────────────────────────────────────

    #[tokio::test]
    async fn get_unknown_pack_404() {
        let app = app(test_state(unique_root("get-unknown")));
        let resp = app
            .clone()
            .oneshot(get_request("/packs/pigeonfish.subtitle-kit"))
            .await
            .unwrap();
        let (status, body) = response_json(resp).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body["error"].as_str().is_some());

        // 非法 id（路径穿越尝试）同样 404，绝不触达文件系统
        let resp = app
            .oneshot(get_request("/packs/..%2F..%2Fetc"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    // ── 6. delete keep_models 查询解析 ────────────────────────────────────

    #[tokio::test]
    async fn delete_keep_models_query_parses() {
        let root = unique_root("delete-keep");
        seed_installed_pack(&root, "test.keep-pack");

        let state = test_state(root.clone());
        let app = app(state);
        let resp = app
            .oneshot(delete_request("/packs/test.keep-pack?keep_models=true"))
            .await
            .unwrap();
        let (status, body) = response_json(resp).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["ok"], true);
        assert!(
            !registry_dir(&root).join("test.keep-pack.json").exists(),
            "卸载后注册文件应删除"
        );
    }

    // ── 7. build 参数校验 → 4xx ───────────────────────────────────────────

    #[tokio::test]
    async fn build_validation_400() {
        let app = app(test_state(unique_root("build-bad")));

        // 空圈选
        let resp = app
            .clone()
            .oneshot(json_request(Method::POST, "/packs/build", json!({})))
            .await
            .unwrap();
        let (status, _) = response_json(resp).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // 非法 pin 语法
        let resp = app
            .clone()
            .oneshot(json_request(
                Method::POST,
                "/packs/build",
                json!({ "models": ["Not A Pin"] }),
            ))
            .await
            .unwrap();
        let (status, _) = response_json(resp).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // 合法 pin 但无匹配模型
        let resp = app
            .clone()
            .oneshot(json_request(
                Method::POST,
                "/packs/build",
                json!({ "models": ["ep.systran.faster-whisper@large-v3"] }),
            ))
            .await
            .unwrap();
        let (status, _) = response_json(resp).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // 未知管线 id
        let resp = app
            .oneshot(json_request(
                Method::POST,
                "/packs/build",
                json!({ "pipelines": ["no-such-pipeline"] }),
            ))
            .await
            .unwrap();
        let (status, _) = response_json(resp).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    // ── 8. export 未构建 → 404 ────────────────────────────────────────────

    #[tokio::test]
    async fn export_not_built_404() {
        let app = app(test_state(unique_root("export-404")));
        let resp = app
            .oneshot(get_request("/packs/pigeonfish.subtitle-kit/export"))
            .await
            .unwrap();
        let (status, body) = response_json(resp).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body["error"].as_str().is_some());
    }

    // ── 9. 未注册子路径 → api 统一 404 ────────────────────────────────────

    #[tokio::test]
    async fn unknown_pack_subroute_falls_to_api_404() {
        let app = app(test_state(unique_root("subroute")));
        let resp = app
            .oneshot(get_request("/packs/some-id/no-such-action"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    // ── 10. pack id 安全校验 ──────────────────────────────────────────────

    #[test]
    fn safe_pack_id_validation() {
        assert!(is_safe_pack_id("pigeonfish.subtitle-kit"));
        assert!(is_safe_pack_id("a.b"));
        assert!(is_safe_pack_id("pub-1.pack-2"));
        assert!(!is_safe_pack_id(""));
        assert!(!is_safe_pack_id("nopublisher"));
        assert!(!is_safe_pack_id("a.b.c"));
        assert!(!is_safe_pack_id("A.b"));
        assert!(!is_safe_pack_id("a/../b"));
        assert!(!is_safe_pack_id("a/b.c"));
        assert!(!is_safe_pack_id("a.b "));
    }

    // ── 11. 模块解析回调（B1 resolve 契约）────────────────────────────────

    const RESOLVE_MANIFEST_TOML: &str = r#"
[module]
id = "test-module"
name = "测试模块"
version = "0.1.0"
description = "resolve test"
category = "asr"
genre = "test"

[runtime]
type = "python"

[compute]
backends = ["cuda", "cpu"]

[[models]]
id = "large-v3"
name = "V3"
source = "huggingface"
repo_id = "org/repo"
target_dir = "td-large"
qualified_id = "ep.test.model"

[[models]]
id = "nourl"
name = "无源"
source = "url"
target_dir = "td-nourl"
qualified_id = "ep.test.nourl"

[interface]
type = "http"
"#;

    fn resolve_manifests() -> Vec<ModuleManifest> {
        vec![toml::from_str(RESOLVE_MANIFEST_TOML).unwrap()]
    }

    #[test]
    fn resolve_entry_variant_match_and_download() {
        let manifests = resolve_manifests();
        let entry = PackModelEntry {
            qualified_id: "ep.test.model".to_string(),
            variant: "large-v3".to_string(),
            mode: ModelMode::Reference,
            tags: vec![],
        };
        let r = resolve_entry(&manifests, &entry).unwrap();
        assert_eq!(r.module_id, "test-module");
        assert_eq!(r.model_id, "large-v3");
        assert_eq!(r.target_dir, "td-large");
        assert_eq!(
            r.backends,
            vec![ComputeBackend::Cuda, ComputeBackend::Cpu]
        );
        let dl = r.download.expect("reference 必须给下载描述符");
        assert_eq!(dl.source, "huggingface");
        assert_eq!(dl.location, "org/repo");

        // bundle 模式无需下载描述符
        let bundle_entry = PackModelEntry {
            mode: ModelMode::Bundle,
            ..entry
        };
        let r = resolve_entry(&manifests, &bundle_entry).unwrap();
        assert!(r.download.is_none());
    }

    #[test]
    fn resolve_entry_failures() {
        let manifests = resolve_manifests();

        // 模块不存在
        let entry = PackModelEntry {
            qualified_id: "ep.ghost.model".to_string(),
            variant: "v1".to_string(),
            mode: ModelMode::Reference,
            tags: vec![],
        };
        assert!(resolve_entry(&manifests, &entry).is_err());

        // 变体不匹配（模块无该变体）
        let entry = PackModelEntry {
            qualified_id: "ep.test.model".to_string(),
            variant: "medium".to_string(),
            mode: ModelMode::Reference,
            tags: vec![],
        };
        assert!(resolve_entry(&manifests, &entry).is_err());

        // reference 但声明缺下载源 → Err（B1 判 Unsupported）
        let entry = PackModelEntry {
            qualified_id: "ep.test.nourl".to_string(),
            variant: "nourl".to_string(),
            mode: ModelMode::Reference,
            tags: vec![],
        };
        assert!(resolve_entry(&manifests, &entry).is_err());
    }

    // ── 12. 适配报告映射（S2 前端形状，仲裁 #3/#4）────────────────────────

    #[tokio::test]
    async fn live_adaptation_shapes() {
        use ep_core::types::{ComputeDevice, DeviceId};
        let manifests = resolve_manifests();
        let devices = vec![ComputeDevice {
            id: DeviceId::Cuda(0),
            backend: ComputeBackend::Cuda,
            name: "Test GPU".to_string(),
            total_memory_mb: Some(8192),
            used_memory_mb: None,
            utilization: None,
            temperature: None,
        }];
        let models = vec![
            ep_pack::import::InstalledPackModel {
                qualified_id: "ep.test.model".to_string(),
                variant: "large-v3".to_string(),
                mode: ModelMode::Reference,
                tags: vec![],
            },
            ep_pack::import::InstalledPackModel {
                qualified_id: "ep.ghost.model".to_string(),
                variant: "v1".to_string(),
                mode: ModelMode::Reference,
                tags: vec![],
            },
        ];
        let out = live_adaptation(&devices, &manifests, &models, "zh-CN");
        assert_eq!(out.len(), 2);
        // 有模块 + cuda 设备命中 → ok + device
        assert!(out[0].ok);
        assert_eq!(out[0].device.as_deref(), Some("cuda:0"));
        assert_eq!(out[0].qualified_id, "ep.test.model");
        // 缺模块 → unsupported
        assert!(!out[1].ok);
        assert!(out[1].device.is_none());

        let v = serde_json::to_value(&out).unwrap();
        assert_eq!(v[0]["ok"], true);
        assert_eq!(v[0]["device"], "cuda:0");
        assert_eq!(v[1]["ok"], false);
    }

    // ── 13. 清单 TOML 输出器往返（渲染 → ep-pack 解析还原）───────────────

    #[test]
    fn render_pack_manifest_roundtrip() {
        let mut notes = std::collections::HashMap::new();
        notes.insert(ComputeBackend::Rocm, "需 torch-rocm wheel".to_string());
        let manifest = PackManifest {
            pack: ep_pack::manifest::PackInfo {
                id: "pigeonfish.subtitle-kit".to_string(),
                version: "1.0.0".to_string(),
                name: "字幕制作整合包".to_string(),
                description: "含\"引号\"与\\反斜杠\n换行".to_string(),
                authors: vec!["pigeonfish".to_string()],
                license: Some("MIT".to_string()),
                homepage: Some("https://example.com/pack".to_string()),
                min_ep_version: Some("0.1.0".to_string()),
                tags: vec!["字幕".to_string(), "视频".to_string()],
            },
            compute: ep_pack::manifest::PackCompute {
                backends: vec![ComputeBackend::Cuda, ComputeBackend::Cpu],
                notes,
            },
            models: vec![PackModelEntry {
                qualified_id: "ep.systran.faster-whisper".to_string(),
                variant: "large-v3".to_string(),
                mode: ModelMode::Bundle,
                tags: vec!["字幕".to_string()],
            }],
            pipelines: vec![PackPipelineRef {
                file: "pipelines/video_to_srt.toml".to_string(),
            }],
        };

        let dir = std::env::temp_dir().join(format!("ep-pack-render-{}", unique_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(ep_pack::extract::MANIFEST_FILE_NAME);
        std::fs::write(&path, render_pack_manifest(&manifest)).unwrap();

        let parsed = PackManifest::from_file(&path).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(parsed, manifest, "TOML 输出器往返应无损");
        assert!(parsed.validate().is_ok());
    }

    // ── 14. WS pack_import 消息形状（经通用 WsMessage 通道）─────────────

    #[tokio::test]
    async fn pack_import_ws_message_shape() {
        let state = test_state(unique_root("ws-shape"));
        let mut rx = state.model_download_tx.subscribe();
        let _ = state.model_download_tx.send(WsMessage::PackImport {
            pack_id: "a.b".to_string(),
            stage: Some("extracting".to_string()),
            percent: Some(35.0),
            state: Some("running".to_string()),
            message: None,
        });
        let msg = rx.recv().await.unwrap();
        let v = serde_json::to_value(&msg).unwrap();
        assert_eq!(v["type"], "pack_import");
        assert_eq!(v["pack_id"], "a.b");
        assert_eq!(v["stage"], "extracting");
        assert_eq!(v["percent"], 35.0);
        assert_eq!(v["state"], "running");
        assert!(v.get("message").is_none());
    }

    // ── 15. reference 下载完成 meta 补丁（仲裁返工）─────────────────────

    /// tags 合并：条目在前、包级在后、去重保序（B1 merge_tags 同款）
    #[test]
    fn merge_pack_tags_dedup_and_order() {
        let entry = vec!["字幕".to_string(), "共享".to_string()];
        let pack = vec!["共享".to_string(), "视频".to_string()];
        assert_eq!(
            merge_pack_tags(&entry, &pack),
            vec!["字幕".to_string(), "共享".to_string(), "视频".to_string()]
        );
        // 两侧均空
        assert!(merge_pack_tags(&[], &[]).is_empty());
        // 仅一侧
        assert_eq!(merge_pack_tags(&[], &pack), pack);
    }

    /// 补丁上下文构造：条目 tags ∪ 包级 tags；清单缺失降级空 tags
    #[test]
    fn ref_meta_patch_builds_from_manifest() {
        let manifest: PackManifest = toml::from_str(
            r#"
[pack]
id = "test.pack"
version = "1.0.0"
name = "n"
description = "d"
tags = ["包级", "共享"]

[compute]
backends = ["cpu"]

[[models]]
qualified_id = "ep.test.model"
variant = "large-v3"
mode = "reference"
tags = ["条目", "共享"]
"#,
        )
        .unwrap();
        let req = ep_pack::import::PendingDownloadRequest {
            qualified_id: "ep.test.model".to_string(),
            variant: "large-v3".to_string(),
            module_id: "test-module".to_string(),
            model_id: "large-v3".to_string(),
            target_dir: "td".to_string(),
            download: ep_pack::import::PendingDownload {
                source: "huggingface".to_string(),
                location: "org/repo".to_string(),
                revision: None,
            },
        };

        let patch = ref_meta_patch(Some(&manifest), "test.pack", &req);
        assert_eq!(patch.pack_id, "test.pack");
        assert_eq!(patch.qualified_id, "ep.test.model");
        assert_eq!(
            patch.tags,
            vec!["条目".to_string(), "共享".to_string(), "包级".to_string()]
        );

        // 清单缺失 → tags 空，但 pack_id/qualified_id 仍在
        let patch_no_manifest = ref_meta_patch(None, "test.pack", &req);
        assert_eq!(patch_no_manifest.pack_id, "test.pack");
        assert!(patch_no_manifest.tags.is_empty());
    }

    /// 下载完成后补丁：meta 带上 pack_id/qualified_id/合并 tags，其余字段保留
    #[tokio::test]
    async fn reference_meta_patch_sets_pack_id() {
        use ep_core::model::ModelMeta;

        let root = unique_root("ref-meta-patch");
        let state = test_state(root.clone());
        let mgr = build_model_manager(&state).await;

        // 预置 ep-core 下载监督任务产出的 meta 形状（pack_id/qualified_id 为 None）
        let target_dir = "ref-model-dir";
        std::fs::create_dir_all(mgr.model_dir(target_dir)).unwrap();
        let base = ModelMeta {
            module_id: "test-module".to_string(),
            model_id: "large-v3".to_string(),
            source: "huggingface".to_string(),
            repo_id: "org/repo".to_string(),
            revision: "main".to_string(),
            downloaded_at: "2026-08-05T00:00:00Z".to_string(),
            total_size_bytes: 1234,
            qualified_id: None,
            tags: vec![],
            pack_id: None,
        };
        mgr.write_meta(target_dir, &base).unwrap();

        let patch = RefMetaPatch {
            pack_id: "test.pack".to_string(),
            qualified_id: "ep.test.model".to_string(),
            tags: vec!["条目".to_string(), "包级".to_string()],
        };
        patch_reference_meta(&state, target_dir, &patch).await;

        let meta = mgr.read_meta(target_dir).expect("meta 应存在");
        assert_eq!(meta.pack_id.as_deref(), Some("test.pack"));
        assert_eq!(meta.qualified_id.as_deref(), Some("ep.test.model"));
        assert_eq!(meta.tags, vec!["条目".to_string(), "包级".to_string()]);
        // 监督任务写入的其余字段保留
        assert_eq!(meta.source, "huggingface");
        assert_eq!(meta.repo_id, "org/repo");
        assert_eq!(meta.total_size_bytes, 1234);
    }

    /// meta 不存在：仅 warn 不 panic（best-effort 语义）
    #[tokio::test]
    async fn reference_meta_patch_missing_meta_is_noop() {
        let state = test_state(unique_root("ref-meta-missing"));
        let patch = RefMetaPatch {
            pack_id: "test.pack".to_string(),
            qualified_id: "ep.test.model".to_string(),
            tags: vec![],
        };
        // 不应 panic；目录不存在同样安全
        patch_reference_meta(&state, "ghost-model-dir", &patch).await;
    }
}
