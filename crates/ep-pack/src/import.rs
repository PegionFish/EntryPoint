//! 整合包导入编排 — 冻结流程见计划 §4.4（暂存 → 解包 → checksum → 清单校验 →
//! 模型 bundle/reference 落位 → 管线冲突处理 → 注册表 → WS pack_import 进度）。
//!
//! 实现所有者：Wave 2 **B1 (PackImport)**。
//!
//! # 库纯度约定
//!
//! daemon 侧路由（`ep-daemon/src/api/packs.rs`）、注册表消费、WS `pack_import`
//! 事件与 URL 下载/上传暂存由 **B2** 接线；CLI（`ep-pack import`）由 C6 接线。
//! 本模块只提供编排核心，**不**直接依赖 ModelManager、不调用设备检测器、
//! 不触网：
//!
//! - 本机设备以已检测的 [`ComputeDevice`] 列表参数注入（§4.6 适配报告输入）；
//! - 模块存在性 / target_dir / 下载源解析经回调注入（[`ResolvedModel`]），
//!   ep-pack 不依赖 ModelManager；
//! - 全部落位路径（models / pipelines / registry / staging）参数注入。
//!
//! # 流程（§4.4）
//!
//! 1. **Extracting** — [`extract_pack`] 解包到 staging 下的唯一子目录
//!    （zip-slip / symlink / 大小上限防护见 [`crate::extract`]）；
//! 2. **Verifying** — [`ChecksumTable::read`] + [`ChecksumTable::verify`]
//!    全量校验（缺失/多余/篡改一次性报告）；
//! 3. **Manifest** — schema 校验 + `min_ep_version` semver 门禁 +
//!    注册表重复安装检查 + 逐模型回调解析与适配报告生成（§4.6）；
//! 4. **Models** — bundle → 落位 `models/<target_dir>`（**TOCTOU 双检**，
//!    绝不合并进已有目录）+ 写 meta（`source="pack"`）+ [`cleanup_hf_cache`]；
//!    reference → 产出待下载描述符（实际下载由上层驱动）；
//! 5. **Pipelines** — TOML 可解析校验 + `[pipeline].id` 提取 +
//!    依赖 `qualified_id@variant` 提取 + 重名冲突报告（覆盖/改名由上层决定）；
//! 6. **Registering** — [`InstalledPack`] 原子写入 `runtime/packs/<pack-id>.json`。
//!
//! 成功后解包子目录整体删除；出错时保留供排查（暂存生命周期归调用方）。

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use ep_core::model::{cleanup_hf_cache, dir_total_size, ModelMeta};
use ep_core::model_id::PinnedModelId;
use ep_core::types::{ComputeBackend, ComputeDevice};

use crate::checksum::{ChecksumError, ChecksumTable};
use crate::extract::{extract_pack, ExtractError, ExtractLimits, MANIFEST_FILE_NAME};
use crate::manifest::{semver, ModelMode, PackManifest, PackManifestError, PackModelEntry};

/// 模型元数据文件名（与 `ep_core::model::META_FILE_NAME`（私有）保持同步）。
const META_FILE_NAME: &str = ".ep_meta.json";

/// bundle 落位 copy 回退路径的临时目录前缀。
const PLACE_TMP_PREFIX: &str = ".ep-import";

/// 解包子目录序号（进程内唯一）。
static EXTRACT_SEQ: AtomicUsize = AtomicUsize::new(0);

// ─── 导入来源 ────────────────────────────────────────────────────────────────

/// 导入来源（对应 §8.1 入口：local / url / upload）。
///
/// serde 形状与 API 请求体契约一致（`POST /api/packs/import`）：
/// `{source:"local",path}` / `{source:"url",url}`；`upload` 变体供
/// `POST /api/packs/upload` 落盘暂存后内部使用。
///
/// URL 来源的网络下载由调用方（daemon B2）先行完成并落到暂存目录，
/// 再以本地路径调用 [`import_pack`]——库层不触网。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "lowercase")]
pub enum ImportSource {
    /// 本地 `.zip` 路径
    Local { path: PathBuf },
    /// 远程 URL（调用方下载进暂存目录后再导入）
    Url { url: String },
    /// 浏览器上传后暂存于 workspace/uploads 的路径
    Upload { path: PathBuf },
}

impl ImportSource {
    /// 已在本地的归档路径（`Local` / `Upload`）；`Url` 返回 None
    /// （须先由调用方下载到暂存目录）。
    pub fn local_path(&self) -> Option<&Path> {
        match self {
            Self::Local { path } | Self::Upload { path } => Some(path),
            Self::Url { .. } => None,
        }
    }
}

// ─── 目标 / 选项 ─────────────────────────────────────────────────────────────

/// 导入落位目标（全部路径参数注入，双平台经 `Path::join` 组装）。
#[derive(Debug, Clone)]
pub struct ImportTargets {
    /// 模型缓存根（bundle 权重落位 `<models_dir>/<target_dir>`；
    /// daemon 侧由 `AppConfig.models.cache_dir` 解析）
    pub models_dir: PathBuf,
    /// 管线目录（`config/pipelines`；重名检测与落位于此）
    pub pipelines_dir: PathBuf,
    /// 已装包注册表目录（`runtime/packs/<pack-id>.json`）
    pub registry_dir: PathBuf,
}

impl ImportTargets {
    /// 默认布局（root 为应用根）：`models/`、`config/pipelines/`、`runtime/packs/`。
    /// daemon 侧应以配置解析后的 `models_dir` 覆盖本构造结果。
    pub fn from_root(root: &Path) -> Self {
        Self {
            models_dir: root.join("models"),
            pipelines_dir: root.join("config").join("pipelines"),
            registry_dir: root.join("runtime").join("packs"),
        }
    }
}

/// 导入选项。
#[derive(Debug, Clone)]
pub struct ImportOptions {
    /// 解包约束（大小上限等，见 [`ExtractLimits`]）
    pub limits: ExtractLimits,
    /// 当前 EntryPoint 版本（`min_ep_version` 门禁用；semver）
    pub current_ep_version: String,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self {
            limits: ExtractLimits::default(),
            // ep-pack 与 EntryPoint 同工作区版本（§4.2 semver 门禁输入）
            current_ep_version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

// ─── 进度（§4.4 WS pack_import 的数据源）─────────────────────────────────────

/// 导入阶段（B2 映射到 WS `pack_import` 消息的 `stage` 字段）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImportStage {
    Extracting,
    Verifying,
    Manifest,
    Models,
    Pipelines,
    Registering,
}

impl ImportStage {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Extracting => "extracting",
            Self::Verifying => "verifying",
            Self::Manifest => "manifest",
            Self::Models => "models",
            Self::Pipelines => "pipelines",
            Self::Registering => "registering",
        }
    }
}

impl std::fmt::Display for ImportStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 进度事件（阶段 + 总百分比 0–100 + 技术层消息；用户文案由 API 层 i18n 映射）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackImportProgress {
    pub stage: ImportStage,
    /// 总进度百分比（0–100，跨阶段单调不减）
    pub percent: u8,
    pub message: String,
}

/// 阶段百分比带宽（总进度切分；阶段内按完成步数线性插值）。
const BAND_EXTRACT: (u8, u8) = (0, 12);
const BAND_VERIFY: (u8, u8) = (12, 38);
const BAND_MANIFEST: (u8, u8) = (38, 45);
const BAND_MODELS: (u8, u8) = (45, 75);
const BAND_PIPELINES: (u8, u8) = (75, 90);
const BAND_REGISTER: (u8, u8) = (90, 100);

/// 带宽内线性插值：`done/total` 步完成时的总百分比（total=0 → 带宽上限）。
fn band_pct(band: (u8, u8), done: usize, total: usize) -> u8 {
    let (lo, hi) = band;
    if total == 0 || done >= total {
        return hi;
    }
    let frac = done as f64 / total as f64;
    (lo as f64 + (hi - lo) as f64 * frac).round() as u8
}

// ─── 模块解析（回调注入契约）─────────────────────────────────────────────────

/// reference 模式的待下载描述符（按模块 manifest 解析得到；
/// 实际下载由上层驱动——复用 DownloadHandle 进度设施，§4.4）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingDownload {
    /// 下载来源："huggingface" | "modelscope" | "url"（对齐 `ModelSource::as_str`）
    pub source: String,
    /// 仓库 ID 或 URL
    pub location: String,
    /// 版本/分支（缺省由下载侧按来源默认值处理）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
}

/// 清单模型条目的解析结果（由调用方回调产出：daemon 查模块 manifest，
/// CLI 按本地模块目录解析）。
#[derive(Debug, Clone)]
pub struct ResolvedModel {
    /// 所属模块 ID（modules/<module_id>）
    pub module_id: String,
    /// 模块 manifest `[[models]].id`（变体）
    pub model_id: String,
    /// 模型缓存相对目录（`[[models]].target_dir`；bundle 落位目标）
    pub target_dir: String,
    /// 模块声明支持的后端（与包 `[compute].backends` 取交集做适配，§4.6）
    pub backends: Vec<ComputeBackend>,
    /// reference 模式的下载描述符；bundle 模式为 None。
    /// reference 模式若为 None → 适配报告判 Unsupported。
    pub download: Option<PendingDownload>,
}

// ─── 适配报告（§4.6）─────────────────────────────────────────────────────────

/// 逐模型适配结论（对齐 S2 前端 `PackAdaptationEntry` 的消费需求：
/// `qualified_id/variant/verdict/reason` + 结论设备 `device`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackAdaptationEntry {
    pub qualified_id: String,
    pub variant: String,
    pub verdict: AdaptationVerdict,
    /// `verdict == Device` 时的结论设备（如 `"cuda:0"`）；其余为 None
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    /// 结论依据（技术层英文；用户可见文案由 API 层经 i18n 映射）
    pub reason: String,
}

/// 适配判定。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdaptationVerdict {
    /// 将运行于指定加速设备（`device` 字段给出，如 cuda:0）
    Device,
    /// CPU 保底
    CpuFallback,
    /// 不支持（原因见 `reason`：缺模块 / 后端交集为空 / 无匹配设备）
    Unsupported,
}

/// 计算单条模型的适配结论（纯函数：清单条目 × 解析结果 × 包后端 × 本机设备）。
///
/// 规则（§4.6）：有效后端 = 包声明 ∩ 模块声明；按包声明顺序找本机已检测
/// 设备（非 CPU 优先）；无加速设备但 CPU 在有效集 → CPU 保底；否则不支持。
pub fn adapt_model(
    entry: &PackModelEntry,
    resolved: &std::result::Result<ResolvedModel, String>,
    pack_backends: &[ComputeBackend],
    devices: &[ComputeDevice],
) -> PackAdaptationEntry {
    let base = PackAdaptationEntry {
        qualified_id: entry.qualified_id.clone(),
        variant: entry.variant.clone(),
        verdict: AdaptationVerdict::Unsupported,
        device: None,
        reason: String::new(),
    };

    let r = match resolved {
        // §4.4 安全模型：缺模块报适配报告而非静默失败
        Err(reason) => {
            return PackAdaptationEntry {
                reason: reason.clone(),
                ..base
            }
        }
        Ok(r) => r,
    };

    // reference 模式必须给出下载描述符，否则无法获得权重
    if entry.mode == ModelMode::Reference && r.download.is_none() {
        return PackAdaptationEntry {
            reason: "reference model: resolver provided no download descriptor".to_string(),
            ..base
        };
    }

    // 有效后端 = 包声明 ∩ 模块声明（保持包声明顺序）
    let effective: Vec<ComputeBackend> = pack_backends
        .iter()
        .copied()
        .filter(|b| r.backends.contains(b))
        .collect();
    if effective.is_empty() {
        return PackAdaptationEntry {
            reason: format!(
                "pack backends [{}] and module backends [{}] are disjoint",
                backends_display(pack_backends),
                backends_display(&r.backends)
            ),
            ..base
        };
    }

    // 按包声明顺序找非 CPU 加速设备
    for backend in effective.iter().filter(|b| **b != ComputeBackend::Cpu) {
        if let Some(dev) = devices.iter().find(|d| d.backend == *backend) {
            return PackAdaptationEntry {
                verdict: AdaptationVerdict::Device,
                device: Some(dev.id.to_string()),
                reason: format!(
                    "backend '{backend}' declared by pack and module; matched local device {}",
                    dev.id
                ),
                ..base
            };
        }
    }

    // 无加速设备：CPU 在有效集 → 保底
    if effective.contains(&ComputeBackend::Cpu) {
        return PackAdaptationEntry {
            verdict: AdaptationVerdict::CpuFallback,
            reason: "no matching accelerator device detected; falling back to CPU".to_string(),
            ..base
        };
    }

    PackAdaptationEntry {
        reason: format!(
            "no local device provides any of the effective backends [{}]",
            backends_display(&effective)
        ),
        ..base
    }
}

fn backends_display(backends: &[ComputeBackend]) -> String {
    backends
        .iter()
        .map(|b| b.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

// ─── 报告与注册表数据结构 ────────────────────────────────────────────────────

/// 已落位 bundle 模型的记录。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledModelRecord {
    pub qualified_id: String,
    pub variant: String,
    /// 模型缓存相对目录
    pub target_dir: String,
    /// 落位后目录总字节（含 meta）
    pub total_bytes: u64,
    /// [`cleanup_hf_cache`] 回收的冗余缓存字节（§4.4 钩子）
    pub cache_bytes_reclaimed: u64,
}

/// reference 模式产出的待下载请求（上层据此驱动下载，复用 DownloadHandle）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingDownloadRequest {
    pub qualified_id: String,
    pub variant: String,
    pub module_id: String,
    pub model_id: String,
    pub target_dir: String,
    #[serde(flatten)]
    pub download: PendingDownload,
}

/// 管线冲突条目（重名 → 上层决定覆盖/改名，库层只报告，§4.4）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PipelineConflict {
    /// 包内相对路径（`/` 分隔）
    pub file: String,
    /// 包内管线的 `[pipeline].id`
    pub pipeline_id: String,
    /// 目标文件名（`pipelines_dir` 下的落位名）
    pub target_file: String,
    /// 冲突原因（id 已存在 / 文件名已存在）
    pub reason: String,
}

/// 注册表条目内的模型记录（形状对齐前端 `PackModelRef`：
/// qualified_id / variant / mode / tags）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledPackModel {
    pub qualified_id: String,
    pub variant: String,
    pub mode: ModelMode,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

/// 已装包注册表条目（`runtime/packs/<pack-id>.json`，§4.4）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledPack {
    pub id: String,
    pub version: String,
    /// 包显示名（来自清单 `[pack].name`；前端 `PackInfo` 列表展示用）。
    /// `serde(default)`：旧注册表 JSON 无此字段可正常加载（None）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// 包描述（来自清单 `[pack].description`）；兼容语义同 [`Self::name`]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// 安装时间（RFC 3339）
    pub installed_at: String,
    /// 包内全部模型条目（bundle 已落位 + reference 待下载均在此）
    pub models: Vec<InstalledPackModel>,
    /// 实际落位的管线 id 列表（冲突未落位者不在其中）
    pub pipelines: Vec<String>,
}

/// 导入结果报告（Ok 路径；硬失败走 [`ImportError`]）。
#[derive(Debug, Clone)]
pub struct ImportReport {
    pub pack_id: String,
    pub version: String,
    pub name: String,
    /// 逐模型适配报告（§4.6，顺序 = 清单 `[[models]]` 顺序）
    pub adaptation: Vec<PackAdaptationEntry>,
    /// 已落位 bundle 模型
    pub installed_models: Vec<InstalledModelRecord>,
    /// reference 模式待下载描述符列表
    pub pending_downloads: Vec<PendingDownloadRequest>,
    /// 已落位管线 id
    pub pipelines_installed: Vec<String>,
    /// 重名冲突管线（未落位；覆盖/改名由上层决定）
    pub pipeline_conflicts: Vec<PipelineConflict>,
    /// 包内全部管线依赖的 pin 列表（`qualified_id@variant`，去重排序）
    pub pipeline_dependencies: Vec<String>,
    /// 非致命警告（hf 缓存清理失败、畸形 pin 等）
    pub warnings: Vec<String>,
    /// 注册表条目落盘路径
    pub registry_path: PathBuf,
    /// `cleanup_hf_cache` 回收字节合计
    pub cache_bytes_reclaimed: u64,
}

// ─── 错误 ────────────────────────────────────────────────────────────────────

/// 导入硬失败（中止全流程；技术层英文消息，用户文案由 API 层 i18n 映射，
/// A4 已提 packs.* 错误键由 B2 在映射层复用）。
#[derive(Debug, Error)]
pub enum ImportError {
    #[error("pack import io error at {}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to serialize/deserialize JSON at {}: {source}", path.display())]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error(transparent)]
    Extract(#[from] ExtractError),
    #[error(transparent)]
    Checksum(#[from] ChecksumError),
    #[error(transparent)]
    Manifest(#[from] PackManifestError),
    #[error("pack requires EntryPoint >= {required}, current version is {current}")]
    MinEpVersion { required: String, current: String },
    #[error("version comparison failed: {detail}")]
    BadVersion { detail: String },
    #[error("pack '{pack_id}' is already installed ({})", path.display())]
    PackAlreadyInstalled { pack_id: String, path: PathBuf },
    #[error("model {qualified_id}@{variant} conflicts with existing directory {}", target.display())]
    ModelConflict {
        qualified_id: String,
        variant: String,
        target: PathBuf,
    },
    #[error("bundle model {qualified_id}@{variant} declares weights but staging lacks models/{target_dir}")]
    BundleMissing {
        qualified_id: String,
        variant: String,
        target_dir: String,
    },
    #[error("manifest references missing pipeline file '{file}'")]
    PipelineFileMissing { file: String },
    #[error("pipeline file '{file}' is invalid: {detail}")]
    InvalidPipeline { file: String, detail: String },
}

type Result<T> = std::result::Result<T, ImportError>;

fn io_err(path: &Path, source: io::Error) -> ImportError {
    ImportError::Io {
        path: path.to_path_buf(),
        source,
    }
}

// ─── 注册表读写（runtime/packs/<pack-id>.json，临时文件 + rename 原子落盘）──

/// 注册表条目路径：`<registry_dir>/<pack-id>.json`。
///
/// pack id 已经清单校验（`<publisher>.<pack-name>`，段字符集
/// `^[a-z0-9][a-z0-9-]*$`），双平台文件名安全。
pub fn registry_entry_path(registry_dir: &Path, pack_id: &str) -> PathBuf {
    registry_dir.join(format!("{pack_id}.json"))
}

/// 读取注册表条目；文件不存在返回 `Ok(None)`。
pub fn read_installed_pack(path: &Path) -> Result<Option<InstalledPack>> {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io_err(path, e)),
    };
    let pack =
        serde_json::from_str(&text).map_err(|source| ImportError::Json {
            path: path.to_path_buf(),
            source,
        })?;
    Ok(Some(pack))
}

/// 原子写入注册表条目（同目录临时文件 + `fs::rename` 替换；
/// 双平台 rename 均为替换语义，不会出现半写文件）。
pub fn write_installed_pack(path: &Path, pack: &InstalledPack) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| io_err(parent, e))?;
    }
    let tmp = path.with_file_name(format!(
        "{}.tmp-{}",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("pack.json"),
        std::process::id()
    ));
    let content =
        serde_json::to_string_pretty(pack).map_err(|source| ImportError::Json {
            path: path.to_path_buf(),
            source,
        })?;
    if let Err(e) = fs::write(&tmp, content) {
        return Err(io_err(&tmp, e));
    }
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp); // best-effort 清理半成品
        return Err(io_err(path, e));
    }
    Ok(())
}

/// 列出注册表目录下全部已装包（目录不存在 → 空列表；
/// 无法解析的条目跳过并忽略——注册表文件由本模块独占写入）。
pub fn list_installed_packs(registry_dir: &Path) -> Result<Vec<InstalledPack>> {
    let entries = match fs::read_dir(registry_dir) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(io_err(registry_dir, e)),
    };
    let mut packs = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        if let Some(pack) = read_installed_pack(&path)? {
            packs.push(pack);
        }
    }
    Ok(packs)
}

// ─── 管线依赖提取（§6.4 依赖清单的包内侧）───────────────────────────────────

/// 从管线 TOML 文档提取模块节点依赖的 pin 列表（`qualified_id@variant`）。
///
/// 仅统计 `kind = "module"` 的节点：pin 字段按冻结契约 §6.2 取 `model`，
/// 兼容现有 dag 形状回退 `model_id`（B7 serde 对齐前的过渡容忍）。
/// builtin/llm 节点的 `model` 参数是外部 API 模型名，不是 qualified id，
/// 一律不提取。畸形 pin 收集进 `warnings` 而非硬失败。
pub fn extract_pipeline_dependencies(doc: &toml::Value, warnings: &mut Vec<String>) -> Vec<String> {
    let mut deps = BTreeSet::new();
    let Some(nodes) = doc.get("nodes").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    for node in nodes {
        let kind = node.get("kind").and_then(|v| v.as_str()).unwrap_or("");
        if kind != "module" {
            continue;
        }
        // §6.2 冻结字段 `model`；回退现 dag.rs 形状 `model_id`
        let pin = node
            .get("model")
            .or_else(|| node.get("model_id"))
            .and_then(|v| v.as_str());
        let Some(pin) = pin else { continue }; // 跟随激活变体（无 pin）
        match PinnedModelId::parse(pin) {
            Ok(pinned) => {
                deps.insert(pinned.to_canonical());
            }
            Err(e) => warnings.push(format!("pipeline node has malformed model pin '{pin}': {e}")),
        }
    }
    deps.into_iter().collect()
}

/// 扫描既有管线目录：`[pipeline].id` → 文件名 映射 + 文件名集合（小写折叠，
/// Windows 大小写不敏感文件系统防覆盖）。无法解析的既有文件跳过。
fn scan_existing_pipelines(pipelines_dir: &Path) -> (BTreeMap<String, String>, BTreeSet<String>) {
    let mut ids: BTreeMap<String, String> = BTreeMap::new();
    let mut names: BTreeSet<String> = BTreeSet::new();
    let Ok(entries) = fs::read_dir(pipelines_dir) else {
        return (ids, names);
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        names.insert(name.to_ascii_lowercase());
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(doc) = text.parse::<toml::Value>() else {
            continue;
        };
        if let Some(id) = doc
            .get("pipeline")
            .and_then(|p| p.get("id"))
            .and_then(|v| v.as_str())
        {
            ids.insert(id.to_string(), name.to_string());
        }
    }
    (ids, names)
}

// ─── 编排主入口 ──────────────────────────────────────────────────────────────

/// 导入编排（§4.4 全流程，同步执行；daemon 侧经 `spawn_blocking` 调用）。
///
/// 参数：
/// - `archive`：已暂存的 `.zip` 归档路径（URL 来源须先由调用方下载）；
/// - `staging_root`：解包隔离区（`.pack-staging`）；本函数在其下创建唯一
///   子目录，成功后整体删除，失败保留供排查；
/// - `targets` / `options`：落位路径与解包约束/版本门禁参数；
/// - `devices`：已检测的本机设备列表（库纯度：不直接调检测器，§4.6）；
/// - `resolve`：模块解析回调（存在性 / target_dir / 下载源注入）；
/// - `progress`：进度回调（B2 映射为 WS `pack_import`）。
///
/// 语义：校验类失败（checksum / 清单 / min_ep_version / 已安装 / bundle 缺失
/// / 模型冲突）在任何落盘动作前或落盘途中硬中止；管线重名与缺模块不中止，
/// 进 [`ImportReport`] 由上层决策。
#[allow(clippy::too_many_arguments)]
pub fn import_pack<R, P>(
    archive: &Path,
    staging_root: &Path,
    targets: &ImportTargets,
    options: &ImportOptions,
    devices: &[ComputeDevice],
    resolve: R,
    progress: P,
) -> Result<ImportReport>
where
    R: Fn(&PackModelEntry) -> std::result::Result<ResolvedModel, String>,
    P: Fn(PackImportProgress),
{
    let emit = |stage: ImportStage, percent: u8, message: String| {
        progress(PackImportProgress {
            stage,
            percent,
            message,
        });
    };

    // ── 1. Extracting ─────────────────────────────────────────────────
    emit(
        ImportStage::Extracting,
        BAND_EXTRACT.0,
        format!("extracting archive {}", archive.display()),
    );
    let extract_dir = staging_root.join(format!(
        "extract-{}-{}",
        std::process::id(),
        EXTRACT_SEQ.fetch_add(1, Ordering::SeqCst)
    ));
    let summary = extract_pack(archive, &extract_dir, &options.limits)?;
    emit(
        ImportStage::Extracting,
        BAND_EXTRACT.1,
        format!(
            "extracted {} files ({} bytes)",
            summary.file_count, summary.total_bytes
        ),
    );

    // 后续任何硬失败都不清理解包目录（保留供排查）；成功路径整体删除
    let outcome = import_after_extract(&extract_dir, targets, options, devices, &resolve, &emit);
    if outcome.is_ok() {
        // bundle 已 rename/复制出去，剩余内容整体丢弃；清理失败不影响导入结果
        let _ = fs::remove_dir_all(&extract_dir);
    }
    outcome
}

/// 解包之后的全部阶段（便于成功清理 / 失败保留的统一出口）。
fn import_after_extract<R, E>(
    extract_dir: &Path,
    targets: &ImportTargets,
    options: &ImportOptions,
    devices: &[ComputeDevice],
    resolve: &R,
    emit: &E,
) -> Result<ImportReport>
where
    R: Fn(&PackModelEntry) -> std::result::Result<ResolvedModel, String>,
    E: Fn(ImportStage, u8, String),
{
    let mut warnings: Vec<String> = Vec::new();

    // ── 2. Verifying：CHECKSUMS 全量校验（先验后落盘，§4.4）─────────────
    emit(
        ImportStage::Verifying,
        BAND_VERIFY.0,
        "reading CHECKSUMS.toml".to_string(),
    );
    let table = ChecksumTable::read(extract_dir)?;
    emit(
        ImportStage::Verifying,
        band_pct(BAND_VERIFY, 1, 2),
        format!("verifying checksums ({} entries)", table.len()),
    );
    table.verify(extract_dir)?;
    emit(
        ImportStage::Verifying,
        BAND_VERIFY.1,
        "checksums verified".to_string(),
    );

    // ── 3. Manifest：schema + semver 门禁 + 重复安装 + 模块解析 + 适配 ──
    emit(
        ImportStage::Manifest,
        BAND_MANIFEST.0,
        "validating manifest".to_string(),
    );
    let manifest = PackManifest::from_file(&extract_dir.join(MANIFEST_FILE_NAME))?;
    if let Err(errors) = manifest.validate() {
        return Err(PackManifestError::Validation(errors).into());
    }
    if let Some(min) = &manifest.pack.min_ep_version {
        match semver::satisfies_min(&options.current_ep_version, min) {
            Ok(true) => {}
            Ok(false) => {
                return Err(ImportError::MinEpVersion {
                    required: min.clone(),
                    current: options.current_ep_version.clone(),
                })
            }
            Err(detail) => return Err(ImportError::BadVersion { detail }),
        }
    }

    // 注册表重复安装检查（早失败：任何落盘动作前）
    let registry_path = registry_entry_path(&targets.registry_dir, &manifest.pack.id);
    if read_installed_pack(&registry_path)?.is_some() {
        return Err(ImportError::PackAlreadyInstalled {
            pack_id: manifest.pack.id.clone(),
            path: registry_path,
        });
    }

    // 模块存在性 / 解析（回调注入）→ 逐模型适配报告（§4.6）
    let resolutions: Vec<std::result::Result<ResolvedModel, String>> =
        manifest.models.iter().map(resolve).collect();
    let adaptation: Vec<PackAdaptationEntry> = manifest
        .models
        .iter()
        .zip(resolutions.iter())
        .map(|(entry, resolved)| {
            adapt_model(entry, resolved, &manifest.compute.backends, devices)
        })
        .collect();
    emit(
        ImportStage::Manifest,
        BAND_MANIFEST.1,
        format!(
            "manifest '{}' v{} validated, {} model(s) resolved",
            manifest.pack.id,
            manifest.pack.version,
            resolutions.iter().filter(|r| r.is_ok()).count()
        ),
    );

    // ── 4. Models：bundle 落位（TOCTOU 双检）+ reference 待下载 ─────────
    let total_models = manifest.models.len();
    let mut installed_models: Vec<InstalledModelRecord> = Vec::new();
    let mut pending_downloads: Vec<PendingDownloadRequest> = Vec::new();
    let mut cache_bytes_reclaimed: u64 = 0;

    // 4a. 冲突预扫描：任何落盘前检查全部 bundle 目标（绝不合并进已有目录）
    for (entry, resolved) in manifest.models.iter().zip(resolutions.iter()) {
        if entry.mode != ModelMode::Bundle {
            continue;
        }
        let Ok(r) = resolved else { continue }; // 缺模块 → 适配报告已列，跳过
        let dst = targets.models_dir.join(&r.target_dir);
        if dst.exists() {
            return Err(ImportError::ModelConflict {
                qualified_id: entry.qualified_id.clone(),
                variant: entry.variant.clone(),
                target: dst,
            });
        }
    }

    // 4b. 逐模型处理
    for (i, (entry, resolved)) in manifest
        .models
        .iter()
        .zip(resolutions.iter())
        .enumerate()
    {
        emit(
            ImportStage::Models,
            band_pct(BAND_MODELS, i, total_models),
            format!(
                "processing model {}@{}",
                entry.qualified_id, entry.variant
            ),
        );
        let Ok(r) = resolved else { continue }; // 缺模块：适配报告已列 Unsupported

        match entry.mode {
            ModelMode::Bundle => {
                let src = extract_dir.join("models").join(&r.target_dir);
                if !src.is_dir() {
                    return Err(ImportError::BundleMissing {
                        qualified_id: entry.qualified_id.clone(),
                        variant: entry.variant.clone(),
                        target_dir: r.target_dir.clone(),
                    });
                }
                let dst = targets.models_dir.join(&r.target_dir);
                place_bundle_dir(&src, &dst, entry)?;

                // 写 meta（source="pack"、pack_id、qualified_id、tags 合并）
                let tags = merge_tags(&entry.tags, &manifest.pack.tags);
                let total_bytes = dir_total_size(&dst);
                let meta = ModelMeta {
                    module_id: r.module_id.clone(),
                    model_id: r.model_id.clone(),
                    source: "pack".to_string(),
                    repo_id: String::new(),
                    revision: String::new(),
                    downloaded_at: chrono::Utc::now().to_rfc3339(),
                    total_size_bytes: total_bytes,
                    qualified_id: Some(entry.qualified_id.clone()),
                    tags,
                    pack_id: Some(manifest.pack.id.clone()),
                };
                write_pack_meta(&dst, &meta)?;

                // §4.4 钩子：落位后清理 HF 缓存冗余副本
                let reclaimed = match cleanup_hf_cache(&dst) {
                    Ok(reclaimed) => {
                        cache_bytes_reclaimed += reclaimed;
                        reclaimed
                    }
                    Err(e) => {
                        warnings.push(format!(
                            "cleanup_hf_cache failed for {}: {e}",
                            dst.display()
                        ));
                        0
                    }
                };

                installed_models.push(InstalledModelRecord {
                    qualified_id: entry.qualified_id.clone(),
                    variant: entry.variant.clone(),
                    target_dir: r.target_dir.clone(),
                    total_bytes: dir_total_size(&dst),
                    cache_bytes_reclaimed: reclaimed,
                });
            }
            ModelMode::Reference => {
                // 适配阶段已拦截无下载描述符的条目
                let Some(download) = r.download.clone() else {
                    continue;
                };
                pending_downloads.push(PendingDownloadRequest {
                    qualified_id: entry.qualified_id.clone(),
                    variant: entry.variant.clone(),
                    module_id: r.module_id.clone(),
                    model_id: r.model_id.clone(),
                    target_dir: r.target_dir.clone(),
                    download,
                });
            }
        }
    }
    emit(
        ImportStage::Models,
        BAND_MODELS.1,
        format!(
            "{} bundle model(s) placed, {} reference model(s) pending download",
            installed_models.len(),
            pending_downloads.len()
        ),
    );

    // ── 5. Pipelines：TOML 校验 + 依赖提取 + 重名冲突 + 落位 ────────────
    let total_pipelines = manifest.pipelines.len();
    let (existing_ids, existing_names) = scan_existing_pipelines(&targets.pipelines_dir);
    let mut pipelines_installed: Vec<String> = Vec::new();
    let mut pipeline_conflicts: Vec<PipelineConflict> = Vec::new();
    let mut pipeline_dependencies: BTreeSet<String> = BTreeSet::new();

    for (i, pref) in manifest.pipelines.iter().enumerate() {
        emit(
            ImportStage::Pipelines,
            band_pct(BAND_PIPELINES, i, total_pipelines),
            format!("processing pipeline file {}", pref.file),
        );

        // 包内相对路径（validate() 已保证 `/` 分隔、无 `..`）→ 逐组件 join
        let src = pref
            .file
            .split('/')
            .filter(|seg| !seg.is_empty() && *seg != ".")
            .fold(extract_dir.to_path_buf(), |acc, seg| acc.join(seg));
        if !src.is_file() {
            return Err(ImportError::PipelineFileMissing {
                file: pref.file.clone(),
            });
        }

        let text = fs::read_to_string(&src).map_err(|e| io_err(&src, e))?;
        let doc: toml::Value = toml::from_str(&text).map_err(|e| ImportError::InvalidPipeline {
            file: pref.file.clone(),
            detail: e.to_string(),
        })?;
        let pipeline_id = doc
            .get("pipeline")
            .and_then(|p| p.get("id"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| ImportError::InvalidPipeline {
                file: pref.file.clone(),
                detail: "missing [pipeline].id".to_string(),
            })?
            .to_string();

        // 依赖提取（含冲突管线：依赖是包级信息，与是否落位无关）
        for dep in extract_pipeline_dependencies(&doc, &mut warnings) {
            pipeline_dependencies.insert(dep);
        }

        let file_name = pref
            .file
            .rsplit('/')
            .find(|seg| !seg.is_empty())
            .unwrap_or(&pref.file)
            .to_string();

        // 重名检测：id 冲突优先；文件名冲突（Windows 大小写折叠）次之
        if let Some(existing_file) = existing_ids.get(&pipeline_id) {
            let reason = format!(
                "pipeline id '{pipeline_id}' already installed ({existing_file})"
            );
            pipeline_conflicts.push(PipelineConflict {
                file: pref.file.clone(),
                pipeline_id,
                target_file: file_name,
                reason,
            });
            continue;
        }
        if existing_names.contains(&file_name.to_ascii_lowercase()) {
            let reason = format!("target file name '{file_name}' already exists");
            pipeline_conflicts.push(PipelineConflict {
                file: pref.file.clone(),
                pipeline_id,
                target_file: file_name,
                reason,
            });
            continue;
        }

        fs::create_dir_all(&targets.pipelines_dir)
            .map_err(|e| io_err(&targets.pipelines_dir, e))?;
        let dst = targets.pipelines_dir.join(&file_name);
        fs::copy(&src, &dst).map_err(|e| io_err(&dst, e))?;
        pipelines_installed.push(pipeline_id);
    }
    emit(
        ImportStage::Pipelines,
        BAND_PIPELINES.1,
        format!(
            "{} pipeline(s) installed, {} conflict(s)",
            pipelines_installed.len(),
            pipeline_conflicts.len()
        ),
    );

    // ── 6. Registering：注册表原子写入 ─────────────────────────────────
    emit(
        ImportStage::Registering,
        BAND_REGISTER.0,
        format!("writing registry entry {}", registry_path.display()),
    );
    let installed = InstalledPack {
        id: manifest.pack.id.clone(),
        version: manifest.pack.version.clone(),
        // 注册表是 GET /api/packs 列表 name/description 的唯一持久数据源
        name: Some(manifest.pack.name.clone()),
        description: Some(manifest.pack.description.clone()),
        installed_at: chrono::Utc::now().to_rfc3339(),
        models: manifest
            .models
            .iter()
            .map(|entry| InstalledPackModel {
                qualified_id: entry.qualified_id.clone(),
                variant: entry.variant.clone(),
                mode: entry.mode,
                tags: entry.tags.clone(),
            })
            .collect(),
        pipelines: pipelines_installed.clone(),
    };
    // 落盘前复查（防并发导入同包穿过 4a 前的早检查）
    if read_installed_pack(&registry_path)?.is_some() {
        return Err(ImportError::PackAlreadyInstalled {
            pack_id: manifest.pack.id.clone(),
            path: registry_path,
        });
    }
    write_installed_pack(&registry_path, &installed)?;
    emit(
        ImportStage::Registering,
        BAND_REGISTER.1,
        format!("pack '{}' registered", manifest.pack.id),
    );

    Ok(ImportReport {
        pack_id: manifest.pack.id.clone(),
        version: manifest.pack.version.clone(),
        name: manifest.pack.name.clone(),
        adaptation,
        installed_models,
        pending_downloads,
        pipelines_installed,
        pipeline_conflicts,
        pipeline_dependencies: pipeline_dependencies.into_iter().collect(),
        warnings,
        registry_path,
        cache_bytes_reclaimed,
    })
}

// ─── bundle 落位 ─────────────────────────────────────────────────────────────

/// 落位 bundle 权重目录：优先同卷 `rename`（零拷贝），跨卷回退
/// 「复制到临时目录 + rename 就位」。
///
/// **TOCTOU 双检**：预扫描（调用方）已查过一次；rename/就位前再查一次。
/// 绝不合并进已有目录——目标存在即 [`ImportError::ModelConflict`]。
fn place_bundle_dir(src: &Path, dst: &Path, entry: &PackModelEntry) -> Result<()> {
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent).map_err(|e| io_err(parent, e))?;
    }

    // 双检之二：落位前再次确认目标不存在
    if dst.exists() {
        return Err(ImportError::ModelConflict {
            qualified_id: entry.qualified_id.clone(),
            variant: entry.variant.clone(),
            target: dst.to_path_buf(),
        });
    }

    if fs::rename(src, dst).is_ok() {
        return Ok(());
    }

    // 跨卷（或权限等）rename 失败 → 复制到暂存同级临时目录，再 rename 就位
    let tmp_name = format!(
        "{}-{}-{}",
        PLACE_TMP_PREFIX,
        dst.file_name().and_then(|n| n.to_str()).unwrap_or("model"),
        std::process::id()
    );
    let tmp = dst.with_file_name(tmp_name);
    if let Err(e) = copy_dir_recursive(src, &tmp) {
        let _ = fs::remove_dir_all(&tmp);
        return Err(e);
    }
    // 就位前第三次确认（rename 到已存在目录在 POSIX 可能替换空目录）
    if dst.exists() {
        let _ = fs::remove_dir_all(&tmp);
        return Err(ImportError::ModelConflict {
            qualified_id: entry.qualified_id.clone(),
            variant: entry.variant.clone(),
            target: dst.to_path_buf(),
        });
    }
    if let Err(e) = fs::rename(&tmp, dst) {
        let _ = fs::remove_dir_all(&tmp);
        return Err(io_err(dst, e));
    }
    Ok(())
}

/// 递归复制目录（仅普通文件/目录；staging 已保证无 symlink，
/// 防御性拒绝其他类型）。
fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst).map_err(|e| io_err(dst, e))?;
    let entries = fs::read_dir(src).map_err(|e| io_err(src, e))?;
    for entry in entries {
        let entry = entry.map_err(|e| io_err(src, e))?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let ft = entry.file_type().map_err(|e| io_err(&from, e))?;
        if ft.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else if ft.is_file() {
            fs::copy(&from, &to).map_err(|e| io_err(&to, e))?;
        } else {
            return Err(io_err(
                &from,
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "non-regular file in pack staging",
                ),
            ));
        }
    }
    Ok(())
}

/// 写 `.ep_meta.json`（pretty JSON，与 `ep_core::model::write_meta_to_dir`
/// 私有实现同语义）。
fn write_pack_meta(dir: &Path, meta: &ModelMeta) -> Result<()> {
    let path = dir.join(META_FILE_NAME);
    let content =
        serde_json::to_string_pretty(meta).map_err(|source| ImportError::Json {
            path: path.clone(),
            source,
        })?;
    fs::write(&path, content).map_err(|e| io_err(&path, e))
}

/// tags 合并：条目 tags 在前、包级 tags 在后，去重保序。
fn merge_tags(entry_tags: &[String], pack_tags: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for tag in entry_tags.iter().chain(pack_tags.iter()) {
        if !out.contains(tag) {
            out.push(tag.clone());
        }
    }
    out
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicUsize, Ordering};

    static TEST_SEQ: AtomicUsize = AtomicUsize::new(0);

    fn unique_root(tag: &str) -> PathBuf {
        let seq = TEST_SEQ.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "ep-pack-import-{tag}-{}-{seq}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn entry(qid: &str, variant: &str, mode: ModelMode) -> PackModelEntry {
        PackModelEntry {
            qualified_id: qid.to_string(),
            variant: variant.to_string(),
            mode,
            tags: Vec::new(),
        }
    }

    fn resolved(backends: Vec<ComputeBackend>, download: Option<PendingDownload>) -> ResolvedModel {
        ResolvedModel {
            module_id: "m".to_string(),
            model_id: "v".to_string(),
            target_dir: "m-v".to_string(),
            backends,
            download,
        }
    }

    fn device(id: ep_core::types::DeviceId, backend: ComputeBackend) -> ComputeDevice {
        ComputeDevice {
            id,
            backend,
            name: "test".to_string(),
            total_memory_mb: None,
            used_memory_mb: None,
            utilization: None,
            temperature: None,
        }
    }

    // ── ImportSource serde（§8.1 请求体形状）──────────────────────────

    #[test]
    fn import_source_serde_matches_api_contract() {
        let local: ImportSource =
            serde_json::from_str(r#"{"source":"local","path":"a.zip"}"#).unwrap();
        assert_eq!(
            local.local_path(),
            Some(Path::new("a.zip")),
        );

        let url: ImportSource =
            serde_json::from_str(r#"{"source":"url","url":"https://x/y.zip"}"#).unwrap();
        assert!(url.local_path().is_none());

        let upload: ImportSource =
            serde_json::from_str(r#"{"source":"upload","path":"u.zip"}"#).unwrap();
        assert_eq!(upload.local_path(), Some(Path::new("u.zip")));

        // 未知 source 拒绝
        assert!(
            serde_json::from_str::<ImportSource>(r#"{"source":"ftp","path":"x"}"#).is_err()
        );
    }

    // ── 适配判定 ───────────────────────────────────────────────────────

    #[test]
    fn adapt_prefers_accelerator_device() {
        let e = entry("ep.a.b", "v1", ModelMode::Bundle);
        let r = Ok(resolved(
            vec![ComputeBackend::Cuda, ComputeBackend::Cpu],
            None,
        ));
        let devices = vec![
            device(ep_core::types::DeviceId::Cpu, ComputeBackend::Cpu),
            device(ep_core::types::DeviceId::Cuda(0), ComputeBackend::Cuda),
        ];
        let a = adapt_model(&e, &r, &[ComputeBackend::Cuda, ComputeBackend::Cpu], &devices);
        assert_eq!(a.verdict, AdaptationVerdict::Device);
        assert_eq!(a.device.as_deref(), Some("cuda:0"));
    }

    #[test]
    fn adapt_cpu_fallback_when_no_accelerator() {
        let e = entry("ep.a.b", "v1", ModelMode::Bundle);
        let r = Ok(resolved(
            vec![ComputeBackend::Cuda, ComputeBackend::Cpu],
            None,
        ));
        let devices = vec![device(ep_core::types::DeviceId::Cpu, ComputeBackend::Cpu)];
        let a = adapt_model(&e, &r, &[ComputeBackend::Cuda, ComputeBackend::Cpu], &devices);
        assert_eq!(a.verdict, AdaptationVerdict::CpuFallback);
        assert!(a.device.is_none());
    }

    #[test]
    fn adapt_unsupported_when_backends_disjoint() {
        let e = entry("ep.a.b", "v1", ModelMode::Bundle);
        let r = Ok(resolved(vec![ComputeBackend::Rocm], None));
        let a = adapt_model(&e, &r, &[ComputeBackend::Cuda], &[]);
        assert_eq!(a.verdict, AdaptationVerdict::Unsupported);
        assert!(a.reason.contains("disjoint"), "{}", a.reason);
    }

    #[test]
    fn adapt_unsupported_when_module_missing() {
        let e = entry("ep.a.b", "v1", ModelMode::Bundle);
        let r: std::result::Result<ResolvedModel, String> =
            Err("module 'b' is not installed".to_string());
        let a = adapt_model(&e, &r, &[ComputeBackend::Cpu], &[]);
        assert_eq!(a.verdict, AdaptationVerdict::Unsupported);
        assert_eq!(a.reason, "module 'b' is not installed");
    }

    #[test]
    fn adapt_reference_requires_download_descriptor() {
        let e = entry("ep.a.b", "v1", ModelMode::Reference);
        let r = Ok(resolved(vec![ComputeBackend::Cpu], None));
        let a = adapt_model(&e, &r, &[ComputeBackend::Cpu], &[]);
        assert_eq!(a.verdict, AdaptationVerdict::Unsupported);
        assert!(a.reason.contains("download descriptor"), "{}", a.reason);
    }

    #[test]
    fn adapt_unsupported_when_no_device_for_effective_backends() {
        let e = entry("ep.a.b", "v1", ModelMode::Bundle);
        let r = Ok(resolved(vec![ComputeBackend::Cuda], None));
        // 有效集 = {cuda}（无 cpu）且本机无 cuda 设备 → 不支持（不静默降级）
        let a = adapt_model(&e, &r, &[ComputeBackend::Cuda], &[]);
        assert_eq!(a.verdict, AdaptationVerdict::Unsupported);
    }

    // ── tags 合并 ──────────────────────────────────────────────────────

    #[test]
    fn merge_tags_dedups_preserving_order() {
        let merged = merge_tags(
            &["asr".to_string(), "demo".to_string()],
            &["demo".to_string(), "视频".to_string()],
        );
        assert_eq!(merged, vec!["asr", "demo", "视频"]);
    }

    // ── 管线依赖提取 ───────────────────────────────────────────────────

    #[test]
    fn pipeline_deps_extract_module_pins_only() {
        let doc: toml::Value = toml::from_str(
            r#"
[pipeline]
id = "p"
name = "P"

[[nodes]]
id = "asr"
kind = "module"
module_id = "asr"
capability = "transcribe"
model = "ep.acme.asr@v1"

[[nodes]]
id = "llm"
kind = "llm"
model = "gpt-4"

[[nodes]]
id = "tts"
kind = "module"
module_id = "tts"
capability = "synthesize"
model_id = "ep.acme.tts@v2"

[[nodes]]
id = "nopin"
kind = "module"
module_id = "asr"
capability = "transcribe"
"#,
        )
        .unwrap();
        let mut warnings = Vec::new();
        let deps = extract_pipeline_dependencies(&doc, &mut warnings);
        assert_eq!(deps, vec!["ep.acme.asr@v1", "ep.acme.tts@v2"]);
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn pipeline_deps_warn_on_malformed_pin() {
        let doc: toml::Value = toml::from_str(
            r#"
[[nodes]]
id = "bad"
kind = "module"
module_id = "asr"
capability = "x"
model = "Not.Valid@v"
"#,
        )
        .unwrap();
        let mut warnings = Vec::new();
        let deps = extract_pipeline_dependencies(&doc, &mut warnings);
        assert!(deps.is_empty());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("Not.Valid@v"), "{warnings:?}");
    }

    #[test]
    fn pipeline_deps_empty_without_nodes() {
        let doc: toml::Value = toml::from_str("[pipeline]\nid = \"p\"\nname = \"P\"\n").unwrap();
        let mut warnings = Vec::new();
        assert!(extract_pipeline_dependencies(&doc, &mut warnings).is_empty());
    }

    // ── 进度带宽 ───────────────────────────────────────────────────────

    #[test]
    fn band_pct_bounds_and_monotonic() {
        assert_eq!(band_pct(BAND_EXTRACT, 0, 0), BAND_EXTRACT.1);
        assert_eq!(band_pct(BAND_MODELS, 0, 4), BAND_MODELS.0);
        assert_eq!(band_pct(BAND_MODELS, 4, 4), BAND_MODELS.1);
        let pcts: Vec<u8> = (0..=4).map(|i| band_pct(BAND_MODELS, i, 4)).collect();
        assert!(pcts.windows(2).all(|w| w[0] <= w[1]), "{pcts:?}");
    }

    // ── 注册表 ─────────────────────────────────────────────────────────

    #[test]
    fn registry_path_and_roundtrip() {
        let root = unique_root("registry");
        let dir = root.join("runtime").join("packs");
        let path = registry_entry_path(&dir, "tester.demo-pack");
        assert_eq!(
            path.file_name().and_then(|n| n.to_str()),
            Some("tester.demo-pack.json")
        );

        assert_eq!(read_installed_pack(&path).unwrap(), None);

        let pack = InstalledPack {
            id: "tester.demo-pack".to_string(),
            version: "1.0.0".to_string(),
            name: Some("Demo Pack".to_string()),
            description: Some("demo description".to_string()),
            installed_at: chrono::Utc::now().to_rfc3339(),
            models: vec![InstalledPackModel {
                qualified_id: "ep.acme.asr".to_string(),
                variant: "v1".to_string(),
                mode: ModelMode::Bundle,
                tags: vec!["asr".to_string()],
            }],
            pipelines: vec!["demo-main".to_string()],
        };
        write_installed_pack(&path, &pack).unwrap();
        assert_eq!(read_installed_pack(&path).unwrap(), Some(pack.clone()));

        // 覆盖写入（升级/重装前提：先卸载）同样原子
        let mut pack2 = pack.clone();
        pack2.version = "1.1.0".to_string();
        write_installed_pack(&path, &pack2).unwrap();
        assert_eq!(read_installed_pack(&path).unwrap(), Some(pack2));

        // 旧版注册表 JSON（无 name/description 字段）向后兼容：读取为 None
        let legacy_path = registry_entry_path(&dir, "legacy.pack");
        std::fs::write(
            &legacy_path,
            r#"{"id":"legacy.pack","version":"0.9.0","installed_at":"2026-01-01T00:00:00Z","models":[],"pipelines":[]}"#,
        )
        .unwrap();
        let legacy = read_installed_pack(&legacy_path).unwrap().unwrap();
        assert_eq!(legacy.id, "legacy.pack");
        assert!(legacy.name.is_none());
        assert!(legacy.description.is_none());

        // 列表：json 之外的文件忽略（demo-pack + legacy 两条）
        std::fs::write(dir.join("README.txt"), b"not a registry entry").unwrap();
        let mut listed = list_installed_packs(&dir).unwrap();
        listed.sort_by(|a, b| a.id.cmp(&b.id));
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].id, "legacy.pack");
        assert_eq!(listed[1].id, "tester.demo-pack");

        // 空目录 / 不存在目录
        assert!(list_installed_packs(&root.join("nope")).unwrap().is_empty());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn registry_json_shape_matches_contract() {
        let pack = InstalledPack {
            id: "a.b".to_string(),
            version: "0.1.0".to_string(),
            name: Some("Pack B".to_string()),
            description: None,
            installed_at: "2026-08-05T00:00:00Z".to_string(),
            models: vec![InstalledPackModel {
                qualified_id: "ep.a.b".to_string(),
                variant: "v".to_string(),
                mode: ModelMode::Reference,
                tags: Vec::new(),
            }],
            pipelines: Vec::new(),
        };
        let json = serde_json::to_value(&pack).unwrap();
        assert_eq!(json["id"], "a.b");
        assert_eq!(json["version"], "0.1.0");
        // name 供前端 PackInfo 列表展示；None 字段不序列化（skip_serializing_if）
        assert_eq!(json["name"], "Pack B");
        assert!(json.get("description").is_none());
        assert_eq!(json["installed_at"], "2026-08-05T00:00:00Z");
        assert_eq!(json["models"][0]["qualified_id"], "ep.a.b");
        assert_eq!(json["models"][0]["mode"], "reference");
        // 空 tags 不序列化（skip_serializing_if）
        assert!(json["models"][0].get("tags").is_none());
        assert_eq!(json["pipelines"].as_array().unwrap().len(), 0);
    }

    // ── 阶段序列化（B2 映射 WS stage 字段）────────────────────────────

    #[test]
    fn stage_serializes_lowercase() {
        assert_eq!(
            serde_json::to_string(&ImportStage::Registering).unwrap(),
            "\"registering\""
        );
        assert_eq!(ImportStage::Models.as_str(), "models");
    }
}
