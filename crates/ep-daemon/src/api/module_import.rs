//! 模块标准档案导入 / 导出 API — HETERO_DIST_PLAN §2.2/§2.3（WS-A M1）。
//!
//! 分发载体为标准压缩档案（zip / tar.gz），无自定义格式：任何"根部（或唯一
//! 一级目录下）含一个合法 `module.toml`"的标准压缩包都可被平台导入；导出则将
//! `modules/<id>/` 原样打回 zip 供迁移分享。
//!
//! # POST /api/modules/import（multipart，单文件字段 `file`）
//!
//! 流程：流式暂存（不整块进内存）→ 安全解包校验 → 清单解析校验 → 版本门禁 →
//! 落位 `modules/<manifest.id>/` → 刷新模块发现表 → 回显 manifest 摘要 + sha256。
//!
//! 安全解包纪律对齐 `ep_pack::extract`（整合包导入器同款基线）：
//! - 拒绝绝对路径（POSIX `/` 与 Windows 盘符）、`..` 分段、反斜杠分隔符、
//!   NUL、Windows 保留设备名；
//! - 拒绝符号链接条目（zip symlink 位 / tar Symlink+HardLink 类型）——归档
//!   不承载链接，杜绝 symlink 逃逸面；
//! - 拒绝非常规文件类型（unix 类型位非 文件/目录/缺省 的 zip 条目与 tar 特殊类型）；
//! - 大小写不敏感的重复条目判重（Windows 大小写折叠会静默互相覆盖）；
//! - 解压字节数流式累计上限（zip 炸弹防御），超限立即中止。
//!
//! 版本门禁（§2.3）：目标 id 已存在时按 semver 比较，仅允许升级；
//! 降级 / 同版 / 现有清单不可比版本一律 409 + 机器可读错误码。
//!
//! 导入信任模型从简：响应回显 manifest 摘要 + 包 sha256，由用户自行判断来源。
//!
//! # GET /api/modules/export/{id}
//!
//! 将 `modules/<id>/` 打包为 zip 下载（排除运行期产物 `__pycache__` / `*.pyc`
//! 等）；包内附带 `SHA256SUMS.txt`（sha256sum -c 兼容格式）；响应头
//! `X-Checksum-Sha256` 为整包哈希。源侧拒绝符号链接与非普通文件（对齐
//! ep-pack 打包纪律：导出必须可无损回环导入）。
//!
//! i18n：复用既有 `apiModels.archive*` / `apiCore.module.*` 键；本端点新增
//! 键统一置于 `apiModules.*` 命名空间（键值落盘前 err_response 按约定回退
//! 返回键本身）。每个错误体额外携带稳定的机器可读 `code` 字段。

use std::collections::HashSet;
use std::io::{BufReader, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use axum::extract::multipart::MultipartRejection;
use axum::extract::{DefaultBodyLimit, Multipart, Path as UrlPath, State};
use axum::http::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;
use tracing::{info, warn};

use ep_core::module::discovery::discover_modules;
use ep_core::module::manifest::ModuleManifest;

use super::err_response;
use super::upload::staging_id;
use crate::state::AppState;

/// 归档内模块清单固定文件名（MODULE_SPEC §1：包根或唯一一级目录下须有 module.toml）
const MODULE_MANIFEST_FILE: &str = "module.toml";

/// 默认解压总字节上限：1 GiB。模块压缩包按 §2.4 纪律永不携带权重
/// （纯代码 + 清单 + 资源），1 GiB 已远超合理体积，同时兜住 zip 炸弹。
const DEFAULT_MAX_TOTAL_BYTES: u64 = 1024 * 1024 * 1024;

/// 解压流式缓冲：256 KiB
const EXTRACT_CHUNK_SIZE: usize = 256 * 1024;

/// unix 文件类型掩码与常量（zip 外部属性里的模式位）
const S_IFMT: u32 = 0o170000;
const S_IFREG: u32 = 0o100000;
const S_IFDIR: u32 = 0o040000;

/// Windows 保留设备名（任意大小写、任意扩展名）：双平台统一拒绝
const WINDOWS_RESERVED_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// 导出时排除的运行期产物目录名
const EXPORT_EXCLUDED_DIRS: &[&str] = &["__pycache__", ".git", ".mypy_cache", ".ruff_cache"];

/// 导出时排除的运行期产物文件后缀 / 文件名
const EXPORT_EXCLUDED_SUFFIXES: &[&str] = &[".pyc", ".pyo"];
const EXPORT_EXCLUDED_FILES: &[&str] = &[".DS_Store"];

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/modules/import", post(import_module))
        .route("/modules/export/{id}", get(export_module))
        // 模块包可能含多平台原生二进制：关闭 axum 默认 2MB body 上限
        // （main.rs 的请求总时长超时中间件已对本路径豁免）
        .layer(DefaultBodyLimit::disable())
}

// ─── 错误表示 ───────────────────────────────────────────────────────────────

/// API 层错误：HTTP 状态码 + i18n 键 + 插值参数 + 机器可读错误码。
///
/// `code` 为稳定契约（不随语言变化）：i18n 键值在 W2 接线前会回退为键本身，
/// 前端/脚本应以 `code` 判定错误类别。
struct ApiError {
    status: StatusCode,
    key: &'static str,
    params: Vec<(&'static str, String)>,
    code: &'static str,
}

macro_rules! api_err {
    ($status:expr, $key:expr, $code:expr) => {
        ApiError { status: $status, key: $key, params: Vec::new(), code: $code }
    };
    ($status:expr, $key:expr, $code:expr; $($k:literal => $v:expr),* $(,)?) => {
        ApiError {
            status: $status,
            key: $key,
            params: vec![$(($k, $v)),*],
            code: $code,
        }
    };
}

impl ApiError {
    fn detail(status: StatusCode, key: &'static str, code: &'static str, detail: impl std::fmt::Display) -> Self {
        Self {
            status,
            key,
            params: vec![("detail", detail.to_string())],
            code,
        }
    }
}

type ApiResult = Result<(StatusCode, Json<Value>), ApiError>;

// ─── 解包错误（跨 spawn_blocking 边界）─────────────────────────────────────

/// 解包/校验阶段错误：全部视为用户提供的归档内容问题 → 400。
#[derive(Debug)]
struct ExtractFailure {
    key: &'static str,
    params: Vec<(&'static str, String)>,
    code: &'static str,
}

impl ExtractFailure {
    fn entry(key: &'static str, code: &'static str, entry: impl std::fmt::Display) -> Self {
        Self {
            key,
            params: vec![("entry", entry.to_string())],
            code,
        }
    }

    fn plain(key: &'static str, code: &'static str) -> Self {
        Self { key, params: Vec::new(), code }
    }
}

impl From<ExtractFailure> for ApiError {
    fn from(f: ExtractFailure) -> Self {
        ApiError {
            status: StatusCode::BAD_REQUEST,
            key: f.key,
            params: f.params,
            code: f.code,
        }
    }
}

/// 解包约束（测试注入小上限用）
#[derive(Debug, Clone, Copy)]
struct ExtractLimits {
    max_total_bytes: u64,
}

impl Default for ExtractLimits {
    fn default() -> Self {
        Self {
            max_total_bytes: DEFAULT_MAX_TOTAL_BYTES,
        }
    }
}

/// 安全解包结果
#[derive(Debug)]
struct ExtractOutcome {
    /// 实际承载模块内容的根目录（staging 内；可能是剥层后的唯一一级目录）
    content_root: PathBuf,
    file_count: usize,
    total_bytes: u64,
}

// ─── 归档分类 ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

// ─── 条目名安全清洗（语义对齐 ep_pack::extract::sanitize_entry_name）───────

/// 清洗归档条目名。
///
/// 拒绝：空名、NUL、任何 `\`、POSIX 绝对路径（`/…`）、Windows 盘符前缀
/// （`C:…`）、`..` 分段、Windows 保留设备名。`.` 与冗余分隔符归一化去掉。
fn sanitize_entry_name(name: &str) -> Option<PathBuf> {
    if name.is_empty() || name.contains('\0') || name.contains('\\') {
        return None;
    }
    let bytes = name.as_bytes();
    if bytes[0] == b'/' {
        return None; // POSIX 绝对路径
    }
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return None; // Windows 盘符前缀
    }

    let mut out = PathBuf::new();
    for seg in name.split('/') {
        match seg {
            "" | "." => {} // 冗余分隔符 / 当前目录分段：忽略
            ".." => return None,
            s => {
                if is_windows_reserved_name(s) {
                    return None;
                }
                out.push(s);
            }
        }
    }
    if out.as_os_str().is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Windows 保留设备名判断（取首段 `.` 之前的 stem，大小写不敏感）
fn is_windows_reserved_name(seg: &str) -> bool {
    let stem = seg.split('.').next().unwrap_or(seg);
    WINDOWS_RESERVED_NAMES
        .iter()
        .any(|r| r.eq_ignore_ascii_case(stem))
}

/// 把相对路径拼到 base 下并保证不越出 base（词法纵深防御）
fn resolve_within(base: &Path, rel: &Path) -> Option<PathBuf> {
    if rel.is_absolute()
        || rel
            .components()
            .any(|c| matches!(c, Component::ParentDir))
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

/// 探测 `base.join(rel)` 的每个祖先组件（base 之下）是否存在符号链接。
///
/// 防御「暂存目录内被预置链接指向外部」的逃逸面（ep-pack 同款检查）。
fn contains_symlink_below(base: &Path, rel: &Path) -> std::io::Result<bool> {
    let mut cur = base.to_path_buf();
    for comp in rel.components() {
        cur.push(comp);
        let meta = match std::fs::symlink_metadata(&cur) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e),
        };
        if meta.file_type().is_symlink() {
            return Ok(true);
        }
    }
    Ok(false)
}

// ─── zip 解包 ───────────────────────────────────────────────────────────────

/// 安全解压 zip 到 dest（同步，供 spawn_blocking 调用）。
fn extract_zip_module(
    archive_path: &Path,
    dest: &Path,
    limits: &ExtractLimits,
) -> Result<ExtractOutcome, ExtractFailure> {
    std::fs::create_dir_all(dest).map_err(staging_fail)?;

    let file = std::fs::File::open(archive_path)
        .map_err(|e| io_fail("apiModels.archiveOpenFailed", e))?;
    let mut archive = zip::ZipArchive::new(BufReader::new(file))
        .map_err(|e| io_fail("apiModels.archiveParseFailed", e))?;

    // 前置校验：module.toml 必须存在（根部或唯一一级目录下），在任何落盘前拒绝。
    // 同时收集根部一级段，用于判定「唯一一层包装目录」。
    preflight_manifest_check_zip(&mut archive)?;

    let mut seen: HashSet<String> = HashSet::new();
    let mut buf = vec![0u8; EXTRACT_CHUNK_SIZE];
    let (mut file_count, mut total_bytes) = (0usize, 0u64);

    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| io_fail("apiModels.archiveEntryReadFailed", e))?;
        let name = entry.name().to_string();

        // 1) 符号链接条目：一律拒绝（归档不承载链接）
        if entry.is_symlink() {
            return Err(ExtractFailure::entry(
                "apiModules.importSymlinkEntry",
                "MODULE_IMPORT_SYMLINK_ENTRY",
                &name,
            ));
        }

        // 2) unix 类型位：仅允许普通文件/目录（缺省 = 0 视为普通，
        //    兼容无 unix 属性的 Windows 归档产出）
        if let Some(mode) = entry.unix_mode() {
            let ft = mode & S_IFMT;
            if ft != 0 && ft != S_IFREG && ft != S_IFDIR {
                return Err(ExtractFailure::entry(
                    "apiModules.importSpecialEntry",
                    "MODULE_IMPORT_SPECIAL_ENTRY",
                    format!("{name} (mode {mode:o})"),
                ));
            }
        }

        // 3) 条目名清洗（主防线）+ enclosed_name 兜底（双层模式）
        let rel = sanitize_entry_name(&name)
            .ok_or_else(|| unsafe_entry(&name))?;
        if entry.enclosed_name().is_none() {
            return Err(unsafe_entry(&name));
        }

        // 4) 拼接后词法边界兜底
        let out = resolve_within(dest, &rel).ok_or_else(|| unsafe_entry(&name))?;

        // 5) 重复条目判重（大小写不敏感：Windows 文件系统大小写折叠）
        let dedup_key = name.trim_end_matches('/').to_ascii_lowercase();
        if !seen.insert(dedup_key) {
            return Err(ExtractFailure::entry(
                "apiModules.importDuplicateEntry",
                "MODULE_IMPORT_DUPLICATE_ENTRY",
                &name,
            ));
        }

        // 6) 祖先 symlink 探测（防预置链接逃逸）
        if contains_symlink_below(dest, &rel).map_err(|e| io_fail("apiModels.uploadMkdirFailed", e))? {
            return Err(unsafe_entry(&name));
        }

        if entry.is_dir() {
            std::fs::create_dir_all(&out)
                .map_err(|e| io_fail("apiModels.uploadMkdirFailed", e))?;
            continue;
        }

        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| io_fail("apiModels.uploadMkdirFailed", e))?;
        }
        let mut writer = std::fs::File::create(&out)
            .map_err(|e| io_fail("apiModels.uploadCreateFileFailed", e))?;

        loop {
            let n = entry
                .read(&mut buf)
                .map_err(|e| entry_io_fail(&name, e))?;
            if n == 0 {
                break;
            }
            let n64 = n as u64;
            // 先判额度再落盘：磁盘累计字节永不超限
            if total_bytes.saturating_add(n64) > limits.max_total_bytes {
                return Err(ExtractFailure {
                    key: "apiModules.importSizeLimitExceeded",
                    params: vec![("limit", limits.max_total_bytes.to_string())],
                    code: "MODULE_IMPORT_SIZE_LIMIT_EXCEEDED",
                });
            }
            writer
                .write_all(&buf[..n])
                .map_err(|e| entry_io_fail(&name, e))?;
            total_bytes += n64;
        }
        file_count += 1;
    }

    Ok(ExtractOutcome {
        content_root: locate_content_root(dest)?,
        file_count,
        total_bytes,
    })
}

/// zip 前置校验：module.toml 必须存在于根部或唯一一级目录下（仅读目录，不解压）
fn preflight_manifest_check_zip(
    archive: &mut zip::ZipArchive<BufReader<std::fs::File>>,
) -> Result<(), ExtractFailure> {
    let mut names: Vec<String> = Vec::with_capacity(archive.len());
    for i in 0..archive.len() {
        let entry = archive
            .by_index(i)
            .map_err(|e| io_fail("apiModels.archiveEntryReadFailed", e))?;
        names.push(entry.name().trim_end_matches('/').to_string());
    }

    let rels: Vec<Vec<String>> = names
        .iter()
        .filter_map(|n| sanitize_entry_name(n).map(|p| path_segments(&p)))
        .collect();

    if !archive_contains_manifest(&rels) {
        return Err(ExtractFailure::plain(
            "apiModules.importManifestMissing",
            "MODULE_IMPORT_MANIFEST_MISSING",
        ));
    }
    Ok(())
}

/// 判定归档是否含 module.toml（根部或唯一一级目录下）
fn archive_contains_manifest(rels: &[Vec<String>]) -> bool {
    if rels
        .iter()
        .any(|s| s.len() == 1 && s[0] == MODULE_MANIFEST_FILE)
    {
        return true;
    }
    // 唯一一层包装目录：<dir>/module.toml 且所有条目共享同一首段
    let first: Option<&String> = rels.first().and_then(|s| s.first());
    match first {
        Some(top) if rels.iter().all(|s| s.first() == Some(top)) => {
            rels.iter()
                .any(|s| s.len() == 2 && s[1] == MODULE_MANIFEST_FILE)
        }
        _ => false,
    }
}

fn path_segments(p: &Path) -> Vec<String> {
    p.components()
        .filter_map(|c| match c {
            Component::Normal(os) => os.to_str().map(str::to_string),
            _ => None,
        })
        .collect()
}

// ─── tar.gz 解包 ────────────────────────────────────────────────────────────

/// 安全解压 .tar.gz / .tgz 到 dest（同步，供 spawn_blocking 调用）。
fn extract_tar_gz_module(
    archive_path: &Path,
    dest: &Path,
    limits: &ExtractLimits,
) -> Result<ExtractOutcome, ExtractFailure> {
    std::fs::create_dir_all(dest).map_err(staging_fail)?;

    let file = std::fs::File::open(archive_path)
        .map_err(|e| io_fail("apiModels.archiveOpenFailed", e))?;
    let decoder = flate2::read::GzDecoder::new(BufReader::new(file));

    // 前置校验：流读一遍条目头做 manifest 存在性 + 全量安全性预检？
    // 权衡：tar 只能顺序读，两遍读需重新解压。改为单遍内先缓冲首个决策——
    // 这里选择直接单遍解包：每条目先做完整安全校验再落盘，任一违规即中止
    // 并由调用方整体丢弃暂存目录（等价安全性，避免双倍解压成本）。
    // manifest 缺失在解包完成后统一判定（locate_content_root 报错），
    // 暂存目录同样整体丢弃，不产生任何持久副作用。
    extract_tar_stream(decoder, dest, limits)
}

/// tar 条目流的安全解包（zip 与 tar.gz 共用的落盘纪律在此收敛）
fn extract_tar_stream<R: Read>(
    reader: R,
    dest: &Path,
    limits: &ExtractLimits,
) -> Result<ExtractOutcome, ExtractFailure> {
    let mut archive = tar::Archive::new(reader);
    let entries = archive
        .entries()
        .map_err(|e| io_fail("apiModels.archiveParseFailed", e))?;

    let mut seen: HashSet<String> = HashSet::new();
    let mut buf = vec![0u8; EXTRACT_CHUNK_SIZE];
    let (mut file_count, mut total_bytes) = (0usize, 0u64);

    for entry in entries {
        let mut entry =
            entry.map_err(|e| io_fail("apiModels.archiveEntryReadFailed", e))?;

        let raw = entry
            .path()
            .map_err(|e| io_fail("apiModels.archiveEntryPathInvalid", e))?
            .to_string_lossy()
            .into_owned();
        let name = raw.trim_end_matches('/').to_string();

        // 1) 条目类型白名单：仅目录 / 普通文件（含 contiguous / GNU sparse）。
        //    符号链接、硬链接、设备、FIFO 等一律拒绝。
        use tar::EntryType;
        let et = entry.header().entry_type();
        match et {
            EntryType::Directory | EntryType::Regular | EntryType::Continuous | EntryType::GNUSparse => {}
            EntryType::Symlink | EntryType::Link => {
                return Err(ExtractFailure::entry(
                    "apiModules.importSymlinkEntry",
                    "MODULE_IMPORT_SYMLINK_ENTRY",
                    &name,
                ));
            }
            other => {
                return Err(ExtractFailure::entry(
                    "apiModules.importSpecialEntry",
                    "MODULE_IMPORT_SPECIAL_ENTRY",
                    format!("{name} ({other:?})"),
                ));
            }
        }

        // 2) 条目名清洗（主防线）
        let rel = sanitize_entry_name(&name)
            .ok_or_else(|| unsafe_entry(name.trim_end_matches('/')))?;

        // 3) 拼接后词法边界兜底
        let out = resolve_within(dest, &rel)
            .ok_or_else(|| unsafe_entry(&name))?;

        // 4) 重复条目判重（大小写不敏感）
        if !seen.insert(name.to_ascii_lowercase()) {
            return Err(ExtractFailure::entry(
                "apiModules.importDuplicateEntry",
                "MODULE_IMPORT_DUPLICATE_ENTRY",
                &name,
            ));
        }

        // 5) 祖先 symlink 探测
        if contains_symlink_below(dest, &rel).map_err(|e| io_fail("apiModels.uploadMkdirFailed", e))? {
            return Err(unsafe_entry(&name));
        }

        if matches!(et, EntryType::Directory) {
            std::fs::create_dir_all(&out)
                .map_err(|e| io_fail("apiModels.uploadMkdirFailed", e))?;
            continue;
        }

        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| io_fail("apiModels.uploadMkdirFailed", e))?;
        }
        let mut writer = std::fs::File::create(&out)
            .map_err(|e| io_fail("apiModels.uploadCreateFileFailed", e))?;

        loop {
            let n = entry
                .read(&mut buf)
                .map_err(|e| entry_io_fail(&name, e))?;
            if n == 0 {
                break;
            }
            let n64 = n as u64;
            if total_bytes.saturating_add(n64) > limits.max_total_bytes {
                return Err(ExtractFailure {
                    key: "apiModules.importSizeLimitExceeded",
                    params: vec![("limit", limits.max_total_bytes.to_string())],
                    code: "MODULE_IMPORT_SIZE_LIMIT_EXCEEDED",
                });
            }
            writer
                .write_all(&buf[..n])
                .map_err(|e| entry_io_fail(&name, e))?;
            total_bytes += n64;
        }
        file_count += 1;
    }

    Ok(ExtractOutcome {
        content_root: locate_content_root(dest)?,
        file_count,
        total_bytes,
    })
}

// ─── 内容根定位与清单解析 ───────────────────────────────────────────────────

/// 定位模块内容根：解包目录根部含 module.toml → 根部本身；
/// 否则恰有一个顶层目录且其中含 module.toml → 该目录（剥一层）。
fn locate_content_root(dest: &Path) -> Result<PathBuf, ExtractFailure> {
    if dest.join(MODULE_MANIFEST_FILE).is_file() {
        return Ok(dest.to_path_buf());
    }

    let entries = std::fs::read_dir(dest)
        .map_err(|e| io_fail("apiModels.uploadStagingFailed", e))?;
    let mut found: Option<PathBuf> = None;
    for e in entries {
        let e = e.map_err(|e| io_fail("apiModels.uploadStagingFailed", e))?;
        let p = e.path();
        if found.is_some() {
            // 多个顶层条目且根部无清单 → 不满足「唯一一级目录」布局
            return Err(ExtractFailure::plain(
                "apiModules.importManifestMissing",
                "MODULE_IMPORT_MANIFEST_MISSING",
            ));
        }
        found = Some(p);
    }

    match found {
        Some(dir) if dir.is_dir() && dir.join(MODULE_MANIFEST_FILE).is_file() => Ok(dir),
        _ => Err(ExtractFailure::plain(
            "apiModules.importManifestMissing",
            "MODULE_IMPORT_MANIFEST_MISSING",
        )),
    }
}

// ─── 落位 ───────────────────────────────────────────────────────────────────

/// 把内容根移动/复制到 `modules/<id>`（阻塞，spawn_blocking 调用）。
///
/// 升级路径：先把既有目录改名到 backup，再移入新内容；中途失败回滚恢复旧目录。
/// 返回是否发生了升级替换。
fn install_blocking(content_root: &Path, target: &Path, backup: &Path) -> Result<bool, String> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create modules dir: {e}"))?;
    }
    if target.is_symlink() {
        return Err(format!(
            "install target {} is a symlink; refusing to overwrite",
            target.display()
        ));
    }

    let existed = target.symlink_metadata().is_ok();
    if existed {
        std::fs::rename(target, backup)
            .map_err(|e| format!("backup existing module: {e}"))?;
    }

    let moved = std::fs::rename(content_root, target).is_ok();
    if !moved {
        // 跨设备回退：递归复制 + 删除暂存
        if let Err(e) = std::fs::create_dir_all(target) {
            restore_backup(backup, target, existed);
            return Err(format!("create target dir: {e}"));
        }
        if let Err(e) = copy_dir_contents(content_root, target) {
            let _ = std::fs::remove_dir_all(target);
            restore_backup(backup, target, existed);
            return Err(format!("copy module contents: {e}"));
        }
        let _ = std::fs::remove_dir_all(content_root);
    }
    Ok(existed)
}

/// 失败回滚：尽力把备份目录还原回目标位
fn restore_backup(backup: &Path, target: &Path, existed: bool) {
    if existed {
        let _ = std::fs::rename(backup, target);
    }
}

/// 递归复制 src 目录内容到 dst（同步）
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
        } else {
            return Err(std::io::Error::other(format!(
                "unexpected non-regular entry during install: {}",
                from.display()
            )));
        }
    }
    Ok(())
}

// ─── Handlers ───────────────────────────────────────────────────────────────

/// POST /api/modules/import — 上传 zip/tar.gz 模块标准档案并导入。
async fn import_module(
    State(state): State<Arc<AppState>>,
    multipart: Result<Multipart, MultipartRejection>,
) -> Response {
    // 错误体在 i18n "error" 之外附加稳定的机器可读 "code" 字段
    match import_module_inner(&state, multipart).await {
        Ok((status, body)) => (status, body).into_response(),
        Err(e) => {
            let (status, Json(mut body)) = err_response(&state, e.status, e.key, &e.params).await;
            if let Some(obj) = body.as_object_mut() {
                obj.insert("code".to_string(), json!(e.code));
            }
            (status, Json(body)).into_response()
        }
    }
}

/// 导入主流程（错误经调用方统一加 code 字段）
async fn import_module_inner(
    state: &Arc<AppState>,
    multipart: Result<Multipart, MultipartRejection>,
) -> ApiResult {
    let mut multipart = match multipart {
        Ok(m) => m,
        Err(e) => {
            warn!(error = %e, "module import: multipart rejected");
            return Err(api_err!(
                StatusCode::BAD_REQUEST,
                "apiModels.uploadMultipartInvalid",
                "MODULE_IMPORT_MULTIPART_INVALID"
            ));
        }
    };

    // ── 阶段 1：流式接收归档到独立 tempdir ──────────────────────────────
    let staging = std::env::temp_dir().join(format!("ep-module-import-{}", staging_id()));
    let parts_dir = staging.join("__parts");
    let extract_dir = staging.join("__extract");
    std::fs::create_dir_all(&parts_dir)
        .map_err(|e| ApiError::detail(StatusCode::INTERNAL_SERVER_ERROR, "apiModels.uploadStagingFailed", "MODULE_IMPORT_INSTALL_FAILED", e))?;

    // RAII 兜底：abort/panic 等异常退出也清理暂存
    struct StagingGuard(PathBuf);
    impl Drop for StagingGuard {
        fn drop(&mut self) {
            if let Err(e) = std::fs::remove_dir_all(&self.0) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    warn!(staging = %self.0.display(), error = %e, "module import staging cleanup failed");
                }
            }
        }
    }
    let _guard = StagingGuard(staging.clone());

    let archive_path = parts_dir.join("archive.part");
    let mut file_name: Option<String> = None;
    let mut saw_file_field = false;
    {
        let mut file = tokio::fs::File::create(&archive_path).await.map_err(|e| {
            ApiError::detail(StatusCode::INTERNAL_SERVER_ERROR, "apiModels.uploadStagingFailed", "MODULE_IMPORT_INSTALL_FAILED", e)
        })?;
        while let Some(mut field) = multipart
            .next_field()
            .await
            .map_err(|e| {
                ApiError::detail(StatusCode::BAD_REQUEST, "apiModels.uploadReadFailed", "MODULE_IMPORT_MULTIPART_INVALID", e)
            })?
        {
            if field.name() != Some("file") {
                continue; // 未知字段跳过
            }
            if saw_file_field {
                continue; // 仅接受第一个 file 字段
            }
            saw_file_field = true;
            file_name = field.file_name().map(str::to_string);
            while let Some(chunk) = field.chunk().await.map_err(|e| {
                ApiError::detail(StatusCode::BAD_REQUEST, "apiModels.uploadReadFailed", "MODULE_IMPORT_MULTIPART_INVALID", e)
            })? {
                file.write_all(&chunk).await.map_err(|e| {
                    ApiError::detail(StatusCode::INTERNAL_SERVER_ERROR, "apiModels.uploadStagingFailed", "MODULE_IMPORT_INSTALL_FAILED", e)
                })?;
            }
        }
        file.flush().await.map_err(|e| {
            ApiError::detail(StatusCode::INTERNAL_SERVER_ERROR, "apiModels.uploadStagingFailed", "MODULE_IMPORT_INSTALL_FAILED", e)
        })?;
    }
    if !saw_file_field {
        return Err(api_err!(
            StatusCode::BAD_REQUEST,
            "apiModels.uploadNoFiles",
            "MODULE_IMPORT_NO_FILE"
        ));
    }
    let raw_name = file_name.unwrap_or_default();
    let kind = classify_archive(&raw_name).ok_or_else(|| {
        api_err!(
            StatusCode::BAD_REQUEST,
            "apiModules.importUnsupportedFormat",
            "MODULE_IMPORT_UNSUPPORTED_FORMAT";
            "name" => raw_name.clone(),
        )
    })?;

    // ── 阶段 2：安全解包（阻塞任务）─────────────────────────────────────
    let limits = ExtractLimits::default();
    let src = archive_path.clone();
    let dst = extract_dir.clone();
    let outcome = tokio::task::spawn_blocking(move || match kind {
        ArchiveKind::Zip => extract_zip_module(&src, &dst, &limits),
        ArchiveKind::TarGz => extract_tar_gz_module(&src, &dst, &limits),
    })
    .await
    .map_err(|e| {
        ApiError::detail(StatusCode::INTERNAL_SERVER_ERROR, "apiModels.uploadExtractPanicked", "MODULE_IMPORT_INSTALL_FAILED", e)
    })?
    .map_err(ApiError::from)?;

    // 整包 sha256（删除归档副本前计算；流式哈希属阻塞 IO，放 spawn_blocking）
    let sha_src = archive_path.clone();
    let archive_sha256 = tokio::task::spawn_blocking(move || ep_pack::checksum::sha256_file(&sha_src))
        .await
        .map_err(|e| {
            ApiError::detail(StatusCode::INTERNAL_SERVER_ERROR, "apiModels.uploadExtractPanicked", "MODULE_IMPORT_INSTALL_FAILED", e)
        })?
        .map_err(|e| ApiError::detail(StatusCode::INTERNAL_SERVER_ERROR, "apiModels.archiveOpenFailed", "MODULE_IMPORT_INSTALL_FAILED", e))?;
    let _ = tokio::fs::remove_file(&archive_path).await;

    // ── 阶段 3：清单解析与校验 ──────────────────────────────────────────
    let manifest_path = outcome.content_root.join(MODULE_MANIFEST_FILE);
    let manifest = ModuleManifest::from_file(&manifest_path).map_err(|e| {
        ApiError::detail(StatusCode::BAD_REQUEST, "apiModules.importManifestInvalid", "MODULE_IMPORT_MANIFEST_INVALID", e)
    })?;
    if let Err(errors) = manifest.validate() {
        return Err(ApiError::detail(
            StatusCode::BAD_REQUEST,
            "apiModules.importManifestInvalid",
            "MODULE_IMPORT_MANIFEST_INVALID",
            errors.join("; "),
        ));
    }

    let module_id = manifest.module.id.clone();
    let incoming_version = manifest.module.version.clone();
    let modules_root = state.root.join("modules");
    let target = modules_root.join(&module_id);

    // ── 阶段 4：版本门禁（仅允许升级；降级/同版 409）────────────────────
    let existing_path = target.join(MODULE_MANIFEST_FILE);
    if existing_path.is_file() {
        let existing = ModuleManifest::from_file(&existing_path);
        let conflict = |detail: String| {
            ApiError::detail(
                StatusCode::CONFLICT,
                "apiModules.importVersionConflict",
                "MODULE_IMPORT_VERSION_CONFLICT",
                detail,
            )
        };
        let existing_version = match existing {
            Ok(mf) => mf.module.version,
            Err(e) => {
                return Err(conflict(format!(
                    "existing module '{module_id}' has unreadable manifest: {e}"
                )));
            }
        };
        match ep_pack::manifest::semver::compare(&incoming_version, &existing_version) {
            Ok(std::cmp::Ordering::Greater) => {} // 升级放行
            Ok(std::cmp::Ordering::Equal) => {
                return Err(conflict(format!(
                    "same version already installed: module '{module_id}' v{existing_version}; \
                     only upgrades are allowed"
                )));
            }
            Ok(std::cmp::Ordering::Less) => {
                return Err(conflict(format!(
                    "downgrade rejected: module '{module_id}' installed v{existing_version}, \
                     incoming v{incoming_version}; only upgrades are allowed"
                )));
            }
            Err(reason) => {
                return Err(conflict(format!(
                    "version comparison failed for module '{module_id}' \
                     (installed v{existing_version}, incoming v{incoming_version}): {reason}"
                )));
            }
        }
    }

    info!(
        module_id = %module_id,
        version = %incoming_version,
        files = outcome.file_count,
        bytes = outcome.total_bytes,
        sha256 = %archive_sha256,
        "API: module archive accepted, installing"
    );

    // ── 阶段 5：落位（备份 → 移入；失败回滚）───────────────────────────
    let backup = staging.join("__old");
    let content_root = outcome.content_root.clone();
    let target_clone = target.clone();
    let upgraded = tokio::task::spawn_blocking(move || {
        install_blocking(&content_root, &target_clone, &backup)
    })
    .await
    .map_err(|e| {
        ApiError::detail(StatusCode::INTERNAL_SERVER_ERROR, "apiModules.importInstallFailed", "MODULE_IMPORT_INSTALL_FAILED", e)
    })?
    .map_err(|detail| {
        ApiError::detail(StatusCode::INTERNAL_SERVER_ERROR, "apiModules.importInstallFailed", "MODULE_IMPORT_INSTALL_FAILED", detail)
    })?;

    // ── 阶段 6：刷新模块发现表（新模块即刻纳入管理）─────────────────────
    refresh_discovered_modules(state).await;

    // ── 阶段 7：响应（manifest 摘要 + sha256，§2.3 信任模型）───────────
    let backends: Vec<String> = manifest
        .compute
        .backends
        .iter()
        .map(|b| b.to_string())
        .collect();
    let summary = json!({
        "status": if upgraded { "upgraded" } else { "imported" },
        "sha256": archive_sha256,
        "file_count": outcome.file_count,
        "total_bytes": outcome.total_bytes,
        "module": {
            "id": manifest.module.id,
            "name": manifest.module.name,
            "version": manifest.module.version,
            "description": manifest.module.description,
            "category": manifest.module.category.to_string(),
            "genre": manifest.module.genre,
            "license": manifest.module.license,
            "backends": backends,
            "path": target.display().to_string(),
        },
    });

    info!(
        module_id = %module_id,
        version = %incoming_version,
        upgraded,
        "API: module import completed"
    );

    Ok((StatusCode::OK, Json(summary)))
}

/// 重新扫描 modules/ 目录并原子替换进程内模块表
async fn refresh_discovered_modules(state: &AppState) {
    let modules_dir = state.root.join("modules");
    match tokio::task::spawn_blocking(move || discover_modules(&modules_dir)).await {
        Ok(discovered) => {
            let count = discovered.len();
            *state.modules.write().await = discovered;
            tracing::debug!(count, "module discovery table refreshed after import");
        }
        Err(e) => {
            warn!(error = %e, "failed to refresh module discovery after import");
        }
    }
}

// ─── 导出 ───────────────────────────────────────────────────────────────────

/// GET /api/modules/export/{id} — 打包 modules/<id>/ 为 zip 下载。
///
/// 响应头：Content-Type application/zip、Content-Disposition attachment、
/// X-Checksum-Sha256（整包 sha256 小写 hex）。包内附带 SHA256SUMS.txt
/// （sha256sum -c 兼容格式）。
async fn export_module(
    State(state): State<Arc<AppState>>,
    UrlPath(id): UrlPath<String>,
) -> Response {
    // id 合法性：模块 id 词表 [a-z0-9-]，兼防路径穿越
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return export_not_found(&state, &id).await;
    }

    let module_dir = state.root.join("modules").join(&id);
    if !module_dir.join(MODULE_MANIFEST_FILE).is_file() {
        return export_not_found(&state, &id).await;
    }

    // 版本用于下载文件名（解析失败不影响导出）
    let version = ModuleManifest::from_file(&module_dir.join(MODULE_MANIFEST_FILE))
        .ok()
        .map(|m| m.module.version)
        .unwrap_or_default();
    let file_stem = if version.is_empty() {
        id.clone()
    } else {
        format!("{id}-{version}")
    };

    // 暂存构建 zip（RAII 清理）
    let staging = std::env::temp_dir().join(format!("ep-module-export-{}", staging_id()));
    if let Err(e) = std::fs::create_dir_all(&staging) {
        warn!(module_id = %id, error = %e, "module export: staging create failed");
        return err_response(
            &state,
            StatusCode::INTERNAL_SERVER_ERROR,
            "apiModules.exportFailed",
            &[("detail", e.to_string())],
        )
        .await
        .into_response();
    }
    struct Guard(PathBuf);
    impl Drop for Guard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _guard = Guard(staging.clone());
    let zip_path = staging.join(format!("{file_stem}.zip"));

    let src = module_dir.clone();
    let out = zip_path.clone();
    let build = tokio::task::spawn_blocking(move || build_export_zip(&src, &out)).await;
    let summary = match build {
        Ok(Ok(s)) => s,
        Ok(Err(detail)) => {
            warn!(module_id = %id, error = %detail, "module export failed");
            let resp = err_response(
                &state,
                StatusCode::INTERNAL_SERVER_ERROR,
                "apiModules.exportFailed",
                &[("detail", detail)],
            )
            .await;
            return resp.into_response();
        }
        Err(e) => {
            let resp = err_response(
                &state,
                StatusCode::INTERNAL_SERVER_ERROR,
                "apiModules.exportFailed",
                &[("detail", e.to_string())],
            )
            .await;
            return resp.into_response();
        }
    };

    // 整包哈希 + 读出字节（代码型模块包通常远小于权重，整读进内存可接受）
    let sha_src = zip_path.clone();
    let sha_result = tokio::task::spawn_blocking(move || ep_pack::checksum::sha256_file(&sha_src))
        .await
        .map_err(|e| e.to_string())
        .and_then(|r| r.map_err(|e| e.to_string()));
    let sha256 = match sha_result {
        Ok(h) => h,
        Err(detail) => {
            let resp = err_response(
                &state,
                StatusCode::INTERNAL_SERVER_ERROR,
                "apiModules.exportFailed",
                &[("detail", detail)],
            )
            .await;
            return resp.into_response();
        }
    };
    let bytes = match tokio::fs::read(&zip_path).await {
        Ok(b) => b,
        Err(e) => {
            let resp = err_response(
                &state,
                StatusCode::INTERNAL_SERVER_ERROR,
                "apiModules.exportFailed",
                &[("detail", e.to_string())],
            )
            .await;
            return resp.into_response();
        }
    };

    info!(module_id = %id, files = summary.file_count, bytes = bytes.len(), %sha256, "API: module exported");

    let mut resp = (StatusCode::OK, bytes).into_response();
    let headers = resp.headers_mut();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/zip"));
    if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{file_stem}.zip\"")) {
        headers.insert(CONTENT_DISPOSITION, v);
    }
    let checksum_header = HeaderName::from_static("x-checksum-sha256");
    if let Ok(val) = HeaderValue::from_str(&sha256) {
        headers.insert(checksum_header, val);
    }
    resp
}

async fn export_not_found(state: &Arc<AppState>, id: &str) -> Response {
    err_response(state, StatusCode::NOT_FOUND, "apiCore.module.notFound", &[("id", id.to_string())])
        .await
        .into_response()
}

/// 导出 zip 构建结果
#[derive(Debug)]
struct ExportSummary {
    file_count: usize,
}

/// 把模块目录打包为 zip（阻塞，spawn_blocking 调用）。
///
/// - 排除运行期产物（__pycache__ / *.pyc 等）；
/// - 拒绝符号链接与非普通文件（导出必须可无损回环导入）；
/// - 附带 SHA256SUMS.txt（sha256sum -c 兼容："<hex>  <rel>"，恒正斜杠）。
fn build_export_zip(module_dir: &Path, out_zip: &Path) -> Result<ExportSummary, String> {
    // 收集文件清单（确定性排序）
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    collect_export_files(module_dir, module_dir, &mut files)?;
    files.sort_by(|a, b| a.0.cmp(&b.0));

    // 生成 SHA256SUMS.txt 内容（逐文件流式哈希）
    let mut sums = String::new();
    for (rel, abs) in &files {
        let hash = ep_pack::checksum::sha256_file(abs)
            .map_err(|e| format!("hash {}: {e}", abs.display()))?;
        sums.push_str(&format!("{hash}  {rel}\n"));
    }

    if let Some(parent) = out_zip.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("staging mkdir: {e}"))?;
    }
    let file = std::fs::File::create(out_zip).map_err(|e| format!("create zip: {e}"))?;
    let mut writer = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .last_modified_time(zip::DateTime::default());

    for (rel, abs) in &files {
        writer
            .start_file(rel.as_str(), options)
            .map_err(|e| format!("zip entry {rel}: {e}"))?;
        let mut f = std::fs::File::open(abs).map_err(|e| format!("open {}: {e}", abs.display()))?;
        std::io::copy(&mut f, &mut writer).map_err(|e| format!("zip write {rel}: {e}"))?;
    }
    writer
        .start_file("SHA256SUMS.txt", options)
        .map_err(|e| format!("zip entry SHA256SUMS.txt: {e}"))?;
    writer
        .write_all(sums.as_bytes())
        .map_err(|e| format!("zip write SHA256SUMS.txt: {e}"))?;
    writer
        .finish()
        .map_err(|e| format!("finalize zip: {e}"))?;

    Ok(ExportSummary {
        file_count: files.len() + 1,
    })
}

/// 递归收集导出文件（相对正斜杠路径 → 绝对路径），排除运行期产物
fn collect_export_files(
    root: &Path,
    dir: &Path,
    out: &mut Vec<(String, PathBuf)>,
) -> Result<(), String> {
    let entries =
        std::fs::read_dir(dir).map_err(|e| format!("read {}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("read {}: {e}", dir.display()))?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let meta = entry
            .file_type()
            .map_err(|e| format!("stat {}: {e}", path.display()))?;
        if meta.is_symlink() {
            return Err(format!(
                "refusing to export symlink (archives must not carry symlinks): {}",
                path.display()
            ));
        }
        let rel = path
            .strip_prefix(root)
            .map_err(|_| format!("path escapes module dir: {}", path.display()))?;
        let rel_str = rel
            .components()
            .map(|c| match c {
                Component::Normal(os) => os.to_string_lossy().into_owned(),
                _ => String::new(),
            })
            .collect::<Vec<_>>()
            .join("/");
        if meta.is_dir() {
            if EXPORT_EXCLUDED_DIRS.contains(&name.as_str()) {
                continue;
            }
            collect_export_files(root, &path, out)?;
        } else if meta.is_file() {
            if EXPORT_EXCLUDED_FILES.contains(&name.as_str())
                || EXPORT_EXCLUDED_SUFFIXES.iter().any(|s| name.ends_with(s))
            {
                continue;
            }
            out.push((rel_str, path));
        } else {
            return Err(format!(
                "refusing to export non-regular file: {}",
                path.display()
            ));
        }
    }
    Ok(())
}

// ─── 错误辅助 ───────────────────────────────────────────────────────────────

fn unsafe_entry(name: &str) -> ExtractFailure {
    ExtractFailure::entry(
        "apiModels.archiveUnsafePath",
        "MODULE_IMPORT_UNSAFE_ENTRY",
        name,
    )
}

/// 归档级 IO/解析失败（detail 携带底层错误；来源可能是 io::Error 或 zip::ZipError）
fn detail_fail(key: &'static str, e: impl std::fmt::Display) -> ExtractFailure {
    ExtractFailure {
        key,
        params: vec![("detail", e.to_string())],
        code: "MODULE_IMPORT_IO_FAILED",
    }
}

/// 暂存目录创建失败（服务侧 IO 问题）
fn staging_fail(e: std::io::Error) -> ExtractFailure {
    detail_fail("apiModels.uploadStagingFailed", e)
}

fn io_fail(key: &'static str, e: impl std::fmt::Display) -> ExtractFailure {
    detail_fail(key, e)
}

fn entry_io_fail(name: &str, e: impl std::fmt::Display) -> ExtractFailure {
    ExtractFailure {
        key: "apiModels.archiveEntryReadFailed",
        params: vec![("name", name.to_string()), ("detail", e.to_string())],
        code: "MODULE_IMPORT_IO_FAILED",
    }
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
    use ep_core::port::PortManager;

    static TEST_SEQ: AtomicUsize = AtomicUsize::new(0);

    const BOUNDARY: &str = "----ep-module-import-test";

    /// 最小合法 native 模块清单（validate() 通过）
    fn manifest_toml(version: &str) -> String {
        format!(
            r#"
[module]
id = "demo-mod"
name = "Demo Module"
version = "{version}"
description = "module import test fixture"
category = "custom"
genre = "test"
license = "MIT"

[runtime]
type = "native"
binaries = {{ "linux-x86_64" = "bin/demo" }}

[compute]
backends = ["cpu"]

[interface]
type = "cli"
"#
        )
    }

    fn unique_root(tag: &str) -> PathBuf {
        let seq = TEST_SEQ.fetch_add(1, Ordering::SeqCst);
        let root =
            std::env::temp_dir().join(format!("ep-modimp-{tag}-{}-{seq}", std::process::id()));
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

    // ── 测试归档构造 ─────────────────────────────────────────────────────

    #[derive(Clone)]
    struct FixtureEntry {
        name: &'static str,
        content: Vec<u8>,
        symlink_target: Option<&'static str>,
        is_dir: bool,
    }

    fn fixture(name: &'static str, content: impl Into<Vec<u8>>) -> FixtureEntry {
        FixtureEntry {
            name,
            content: content.into(),
            symlink_target: None,
            is_dir: false,
        }
    }

    fn symlink(name: &'static str, target: &'static str) -> FixtureEntry {
        FixtureEntry {
            name,
            content: Vec::new(),
            symlink_target: Some(target),
            is_dir: false,
        }
    }

    /// 用 ZipWriter 写 zip（start_file 不清洗名字，可构造恶意条目）
    fn build_zip(entries: &[FixtureEntry]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        for e in entries {
            if let Some(target) = e.symlink_target {
                writer.add_symlink(e.name, target, options).unwrap();
            } else if e.is_dir {
                writer.add_directory(e.name, options).unwrap();
            } else {
                writer.start_file(e.name, options).unwrap();
                std::io::Write::write_all(&mut writer, &e.content).unwrap();
            }
        }
        writer.finish().unwrap().into_inner()
    }

    /// 内存构造 .tar.gz
    fn build_tar_gz(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (name, data) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, *name, *data).unwrap();
        }
        let tar_bytes = builder.into_inner().unwrap();
        let mut encoder =
            flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut encoder, &tar_bytes).unwrap();
        encoder.finish().unwrap()
    }

    // ── multipart 请求构造 ───────────────────────────────────────────────

    fn form_part(buf: &mut Vec<u8>, filename: &str, data: &[u8]) {
        buf.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
        buf.extend_from_slice(
            format!(
                "Content-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\n\
                 Content-Type: application/octet-stream\r\n\r\n"
            )
            .as_bytes(),
        );
        buf.extend_from_slice(data);
        buf.extend_from_slice(b"\r\n");
    }

    fn finish_multipart(buf: &mut Vec<u8>) {
        buf.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
    }

    fn import_request(body: Vec<u8>) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri("/modules/import")
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

    async fn import_zip(state: Arc<AppState>, zip_bytes: Vec<u8>) -> (StatusCode, Value) {
        let mut body = Vec::new();
        form_part(&mut body, "demo-mod-1.0.0.zip", &zip_bytes);
        finish_multipart(&mut body);
        let app = router().with_state(state);
        let resp = app.oneshot(import_request(body)).await.unwrap();
        response_json(resp).await
    }

    fn valid_module_zip(version: &str, extra: &[FixtureEntry]) -> Vec<u8> {
        let mut entries = vec![fixture("module.toml", manifest_toml(version))];
        entries.push(fixture("README.md", "hello".as_bytes().to_vec()));
        entries.extend(extra.iter().cloned());
        build_zip(&entries)
    }

    // ── 正常导入落位 ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn import_zip_success_places_module_and_refreshes_state() {
        let root = unique_root("ok");
        let state = test_state(root.clone());

        let zip_bytes = valid_module_zip("0.1.0", &[fixture("bin/demo", b"#!/bin/sh\n")]);
        let (status, json) = import_zip(state.clone(), zip_bytes).await;

        assert_eq!(status, StatusCode::OK, "响应: {json}");
        assert_eq!(json["status"], "imported");
        assert_eq!(json["module"]["id"], "demo-mod");
        assert_eq!(json["module"]["version"], "0.1.0");
        assert_eq!(json["module"]["license"], "MIT");
        assert_eq!(json["module"]["backends"], serde_json::json!(["cpu"]));
        assert_eq!(json["file_count"], serde_json::json!(3)); // module.toml + README.md + bin/demo
        // sha256：64 位小写 hex
        let sha = json["sha256"].as_str().unwrap();
        assert_eq!(sha.len(), 64);
        assert!(sha.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));

        // 落位断言
        let target = root.join("modules/demo-mod");
        assert_eq!(std::fs::read(target.join("README.md")).unwrap(), b"hello");
        assert_eq!(std::fs::read(target.join("bin/demo")).unwrap(), b"#!/bin/sh\n");
        assert!(target.join("module.toml").is_file());

        // 进程内模块表已刷新（新模块即刻可见）
        let modules = state.modules.read().await;
        let found = modules
            .iter()
            .find(|m| m.manifest.as_ref().map(|mf| mf.module.id == "demo-mod").unwrap_or(false));
        assert!(found.is_some(), "导入后 state.modules 应包含 demo-mod");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// 包根带唯一一层包装目录 → 剥层落位
    #[tokio::test]
    async fn import_zip_wrapped_single_top_dir_strips_layer() {
        let root = unique_root("wrap");
        let state = test_state(root.clone());

        let zip_bytes = build_zip(&[
            fixture("demo-mod-0.1.0/module.toml", manifest_toml("0.1.0")),
            fixture("demo-mod-0.1.0/adapter.cfg", b"x=1"),
        ]);
        let (status, json) = import_zip(state.clone(), zip_bytes).await;

        assert_eq!(status, StatusCode::OK, "响应: {json}");
        let target = root.join("modules/demo-mod");
        assert!(target.join("module.toml").is_file(), "清单应位于模块根");
        assert_eq!(std::fs::read(target.join("adapter.cfg")).unwrap(), b"x=1");
        assert!(!root.join("modules/demo-mod-0.1.0").exists(), "包装目录应被剥掉");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// tar.gz 同样支持
    #[tokio::test]
    async fn import_tar_gz_success() {
        let root = unique_root("tgz");
        let state = test_state(root.clone());

        let tgz = build_tar_gz(&[
            ("module.toml", manifest_toml("0.1.0").as_bytes()),
            ("assets/logo.svg", b"<svg/>"),
        ]);

        let mut body = Vec::new();
        form_part(&mut body, "demo.tar.gz", &tgz);
        finish_multipart(&mut body);
        let app = router().with_state(state);
        let resp = app.oneshot(import_request(body)).await.unwrap();
        let (status, json) = response_json(resp).await;

        assert_eq!(status, StatusCode::OK, "响应: {json}");
        let target = root.join("modules/demo-mod");
        assert!(target.join("module.toml").is_file());
        assert_eq!(std::fs::read(target.join("assets/logo.svg")).unwrap(), b"<svg/>");
        let _ = std::fs::remove_dir_all(&root);
    }

    // ── 攻击矩阵：zip-slip / 符号链接 / 非常规类型 / 重复条目 ───────────

    #[tokio::test]
    async fn import_rejects_zip_slip_entries() {
        let root = unique_root("zipslip");
        let state = test_state(root.clone());

        let zip_bytes = build_zip(&[
            fixture("module.toml", manifest_toml("0.1.0")),
            fixture("../evil.txt", b"pwned"),
            fixture("nested/../../evil2.txt", b"pwned2"),
        ]);
        let (status, json) = import_zip(state.clone(), zip_bytes).await;

        assert_eq!(status, StatusCode::BAD_REQUEST, "{json}");
        assert_eq!(json["code"], "MODULE_IMPORT_UNSAFE_ENTRY");
        // 逃逸目标绝不出现，模块未落位
        assert!(!root.join("evil.txt").exists());
        assert!(!root.join("evil2.txt").exists());
        assert!(!root.join("modules/demo-mod").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn import_rejects_absolute_and_backslash_entries() {
        let root = unique_root("abs");
        let state = test_state(root.clone());

        let zip_bytes = build_zip(&[
            fixture("module.toml", manifest_toml("0.1.0")),
            fixture("/etc/passwd", b"root::0:0"),
        ]);
        let (status, json) = import_zip(state.clone(), zip_bytes).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(json["code"], "MODULE_IMPORT_UNSAFE_ENTRY");
        assert!(!root.join("etc").exists());

        let zip_bytes = build_zip(&[
            fixture("module.toml", manifest_toml("0.1.0")),
            fixture("dir\\file.txt", b"backslash"),
        ]);
        let (status, json) = import_zip(state.clone(), zip_bytes).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(json["code"], "MODULE_IMPORT_UNSAFE_ENTRY");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn import_rejects_symlink_entry() {
        let root = unique_root("symlink");
        let state = test_state(root.clone());

        let zip_bytes = build_zip(&[
            fixture("module.toml", manifest_toml("0.1.0")),
            symlink("models/link", "../../../outside-target"),
        ]);
        let (status, json) = import_zip(state.clone(), zip_bytes).await;

        assert_eq!(status, StatusCode::BAD_REQUEST, "{json}");
        assert_eq!(json["code"], "MODULE_IMPORT_SYMLINK_ENTRY");
        assert!(!root.join("modules/demo-mod").exists());
        assert!(!root.join("outside-target").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn import_rejects_duplicate_case_insensitive_entries() {
        let root = unique_root("dup");
        let state = test_state(root.clone());

        // ZipWriter 拒绝重名条目 → 仅大小写不同的条目须字节级构造
        // （Windows 大小写折叠文件系统上它们会静默互相覆盖）
        let zip_bytes = raw_zip(&[
            RawEntry { name: "module.toml", data: manifest_toml("0.1.0").into_bytes(), unix_mode: 0o100644 },
            RawEntry { name: "README.md", data: b"first".to_vec(), unix_mode: 0o100644 },
            RawEntry { name: "readme.MD", data: b"second".to_vec(), unix_mode: 0o100644 },
        ]);
        let (status, json) = import_zip(state.clone(), zip_bytes).await;

        assert_eq!(status, StatusCode::BAD_REQUEST, "{json}");
        assert_eq!(json["code"], "MODULE_IMPORT_DUPLICATE_ENTRY");
        assert!(!root.join("modules/demo-mod").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn import_rejects_special_file_entries() {
        let root = unique_root("fifo");
        let state = test_state(root.clone());

        let zip_bytes = raw_zip(&[
            RawEntry { name: "module.toml", data: manifest_toml("0.1.0").into_bytes(), unix_mode: 0o100644 },
            RawEntry { name: "pipe", data: Vec::new(), unix_mode: 0o010644 }, // S_IFIFO
        ]);
        let (status, json) = import_zip(state.clone(), zip_bytes).await;

        assert_eq!(status, StatusCode::BAD_REQUEST, "{json}");
        assert_eq!(json["code"], "MODULE_IMPORT_SPECIAL_ENTRY");
        assert!(!root.join("modules/demo-mod").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    // ── 大小上限 ─────────────────────────────────────────────────────────

    #[test]
    fn extract_enforces_size_limit_while_streaming() {
        let root = unique_root("sizelimit");
        let archive = root.join("big.zip");
        std::fs::write(
            &archive,
            build_zip(&[
                fixture("module.toml", manifest_toml("0.1.0")),
                fixture("a.bin", vec![0xABu8; 64 * 1024]),
                fixture("b.bin", vec![0xABu8; 64 * 1024]),
            ]),
        )
        .unwrap();
        let dest = root.join("extract");
        let limits = ExtractLimits {
            max_total_bytes: 100 * 1024,
        };
        let err = extract_zip_module(&archive, &dest, &limits).unwrap_err();
        assert_eq!(err.code, "MODULE_IMPORT_SIZE_LIMIT_EXCEEDED");
        // 先判额度再落盘：磁盘占用不超限
        assert!(dir_size(&dest) <= limits.max_total_bytes);
        let _ = std::fs::remove_dir_all(&root);
    }

    // ── 清单缺失 / 非法 ─────────────────────────────────────────────────

    #[tokio::test]
    async fn import_rejects_archive_without_manifest() {
        let root = unique_root("no-manifest");
        let state = test_state(root.clone());

        let zip_bytes = build_zip(&[fixture("readme.txt", b"no manifest here")]);
        let (status, json) = import_zip(state.clone(), zip_bytes).await;

        assert_eq!(status, StatusCode::BAD_REQUEST, "{json}");
        assert_eq!(json["code"], "MODULE_IMPORT_MANIFEST_MISSING");
        assert!(!root.join("modules").join("demo-mod").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn import_rejects_invalid_manifest() {
        let root = unique_root("bad-manifest");
        let state = test_state(root.clone());

        // validate() 会拒绝：python 运行时缺 python_version
        let bad_toml = r#"
[module]
id = "demo-mod"
name = "Demo"
version = "0.1.0"
description = "d"
category = "asr"
genre = "t"

[runtime]
type = "python"

[compute]
backends = ["cpu"]

[interface]
type = "http"
"#;
        let zip_bytes = build_zip(&[fixture("module.toml", bad_toml)]);
        let (status, json) = import_zip(state.clone(), zip_bytes).await;

        assert_eq!(status, StatusCode::BAD_REQUEST, "{json}");
        assert_eq!(json["code"], "MODULE_IMPORT_MANIFEST_INVALID");
        assert!(!root.join("modules/demo-mod").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn import_rejects_unsupported_extension() {
        let root = unique_root("ext");
        let state = test_state(root.clone());

        let mut body = Vec::new();
        form_part(&mut body, "payload.rar", b"not-an-archive-we-support");
        finish_multipart(&mut body);
        let app = router().with_state(state);
        let resp = app.oneshot(import_request(body)).await.unwrap();
        let (status, json) = response_json(resp).await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(json["code"], "MODULE_IMPORT_UNSUPPORTED_FORMAT");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn import_without_file_field_400() {
        let root = unique_root("nofile");
        let state = test_state(root.clone());

        // 仅非 file 字段 → 缺文件
        let mut body = Vec::new();
        body.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
        body.extend_from_slice(b"Content-Disposition: form-data; name=\"unrelated\"\r\n\r\nx\r\n");
        finish_multipart(&mut body);
        let app = router().with_state(state);
        let resp = app.oneshot(import_request(body)).await.unwrap();
        let (status, json) = response_json(resp).await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(json["code"], "MODULE_IMPORT_NO_FILE");
        let _ = std::fs::remove_dir_all(&root);
    }

    // ── 版本门禁：降级 / 同版 409，升级放行 ─────────────────────────────

    #[tokio::test]
    async fn import_downgrade_rejected_409() {
        let root = unique_root("downgrade");
        let state = test_state(root.clone());

        // 预装 v0.9.0（带标记文件证明未被破坏）
        let target = root.join("modules/demo-mod");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("module.toml"), manifest_toml("0.9.0")).unwrap();
        std::fs::write(target.join("installed.marker"), b"keep").unwrap();

        let (status, json) = import_zip(state, valid_module_zip("0.1.0", &[])).await;
        assert_eq!(status, StatusCode::CONFLICT, "{json}");
        assert_eq!(json["code"], "MODULE_IMPORT_VERSION_CONFLICT");
        // 既有模块原样保留
        assert_eq!(
            std::fs::read(target.join("installed.marker")).unwrap(),
            b"keep"
        );
        assert!(target.join("module.toml").is_file());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn import_same_version_rejected_409() {
        let root = unique_root("samever");
        let state = test_state(root.clone());

        let target = root.join("modules/demo-mod");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("module.toml"), manifest_toml("0.1.0")).unwrap();

        let (status, json) = import_zip(state, valid_module_zip("0.1.0", &[])).await;
        assert_eq!(status, StatusCode::CONFLICT, "{json}");
        assert_eq!(json["code"], "MODULE_IMPORT_VERSION_CONFLICT");
        assert!(!target.join("README.md").exists(), "同版拒绝不得改动既有目录");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn import_upgrade_replaces_directory() {
        let root = unique_root("upgrade");
        let state = test_state(root.clone());

        let target = root.join("modules/demo-mod");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("module.toml"), manifest_toml("0.1.0")).unwrap();
        std::fs::write(target.join("old-only.txt"), b"stale").unwrap();

        let (status, json) =
            import_zip(state, valid_module_zip("0.2.0", &[fixture("new-file.txt", b"fresh")])).await;

        assert_eq!(status, StatusCode::OK, "{json}");
        assert_eq!(json["status"], "upgraded");
        assert_eq!(json["module"]["version"], "0.2.0");
        assert_eq!(std::fs::read(target.join("new-file.txt")).unwrap(), b"fresh");
        assert!(!target.join("old-only.txt").exists(), "旧版独有文件应随升级移除");
        assert!(target.join("module.toml").is_file());
        let _ = std::fs::remove_dir_all(&root);
    }

    // ── 导出 ─────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn export_roundtrip_returns_zip_with_checksum_header() {
        let root = unique_root("export");
        let state = test_state(root.clone());

        // 先导入一个模块
        let (status, _) = import_zip(
            state.clone(),
            valid_module_zip("0.1.0", &[fixture("bin/demo", b"payload-bytes")]),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let app = router().with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/modules/export/demo-mod")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()["content-type"],
            "application/zip"
        );
        let checksum = resp
            .headers()
            .get("x-checksum-sha256")
            .expect("X-Checksum-Sha256 header")
            .to_str()
            .unwrap()
            .to_string();
        assert_eq!(checksum.len(), 64);

        let disposition = resp.headers()["content-disposition"]
            .to_str()
            .unwrap()
            .to_string();
        assert!(disposition.contains("demo-mod-0.1.0.zip"), "{disposition}");

        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&bytes[..2], b"PK", "应为 zip 魔数");

        // 头部哈希 = 包字节哈希
        let tmp = root.join("downloaded.zip");
        std::fs::write(&tmp, &bytes).unwrap();
        assert_eq!(
            ep_pack::checksum::sha256_file(&tmp).unwrap(),
            checksum
        );

        // 包内容回环：可解开且含 module.toml + SHA256SUMS.txt + 全部源文件
        let mut archive =
            zip::ZipArchive::new(std::io::Cursor::new(bytes.clone())).unwrap();
        assert!(archive.by_name("module.toml").is_ok());
        assert!(archive.by_name("SHA256SUMS.txt").is_ok());
        assert!(archive.by_name("README.md").is_ok());
        assert!(archive.by_name("bin/demo").is_ok());
        let _ = std::fs::remove_dir_all(&tmp);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn build_export_zip_produces_sha256sums_and_excludes_runtime_artifacts() {
        let root = unique_root("export-build");
        let module_dir = root.join("demo-mod");
        std::fs::create_dir_all(module_dir.join("bin")).unwrap();
        std::fs::create_dir_all(module_dir.join("__pycache__")).unwrap();
        std::fs::write(module_dir.join("module.toml"), manifest_toml("0.1.0")).unwrap();
        std::fs::write(module_dir.join("adapter.pyc"), b"cachable-junk").unwrap();
        std::fs::write(module_dir.join("__pycache__/x.cpython-311.pyc"), b"junk").unwrap();
        std::fs::write(module_dir.join("bin/demo"), b"binary-payload").unwrap();

        let out = root.join("out.zip");
        let summary = build_export_zip(&module_dir, &out).unwrap();
        // module.toml + bin/demo + SHA256SUMS.txt（*.pyc 与 __pycache__ 排除）
        assert_eq!(summary.file_count, 3);

        let mut archive = zip::ZipArchive::new(BufReader::new(std::fs::File::open(&out).unwrap())).unwrap();
        assert!(archive.by_name("module.toml").is_ok());
        assert!(archive.by_name("bin/demo").is_ok());
        assert!(archive.by_name("adapter.pyc").is_err(), "运行期产物应被排除");
        assert!(archive.by_name("__pycache__/x.cpython-311.pyc").is_err());

        let sums_entry = archive.by_name("SHA256SUMS.txt").unwrap();
        let mut text = String::new();
        BufReader::new(sums_entry)
            .read_to_string(&mut text)
            .unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "SUMS 应恰好覆盖两个文件: {text}");
        for line in &lines {
            let (hash, path) = line.split_once("  ").expect("sha256sum -c 格式");
            assert_eq!(hash.len(), 64);
            assert!(path == "bin/demo" || path == "module.toml", "{line}");
        }

        #[cfg(unix)]
        {
            // 符号链接拒绝（导出必须可无损回环导入）
            std::os::unix::fs::symlink("/etc/passwd", module_dir.join("evil-link")).unwrap();
            let err = build_export_zip(&module_dir, &root.join("out2.zip")).unwrap_err();
            assert!(err.contains("symlink"), "{err}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn export_unknown_module_404() {
        let root = unique_root("export-404");
        let state = test_state(root.clone());

        let app = router().with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/modules/export/ghost-mod")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["error"], "模块不存在：ghost-mod");

        // 路径穿越 id 一律 404（id 词表门禁）
        let state2 = test_state(root.clone());
        let app = router().with_state(state2);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/modules/export/..%2Fconfig")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        // URL 编码会被 axum 解码为 ".."，词表拒绝 → 404
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let _ = std::fs::remove_dir_all(&root);
    }

    // ── 条目名清洗纯函数矩阵 ─────────────────────────────────────────────

    #[test]
    fn sanitize_entry_name_rejects_unsafe_names() {
        for bad in [
            "",
            "../evil",
            "a/../b",
            "/abs/path",
            "C:/win/path",
            "C:relative",
            "back\\slash",
            "\0nul",
            "CON",
            "aux.txt",
            "com1.log",
            ".",
            "././",
        ] {
            assert!(
                sanitize_entry_name(bad).is_none(),
                "应拒绝 {bad:?}"
            );
        }
    }

    #[test]
    fn sanitize_entry_name_normalizes_safe_names() {
        assert_eq!(
            sanitize_entry_name("a/b/c.toml").unwrap(),
            PathBuf::from("a/b/c.toml")
        );
        assert_eq!(sanitize_entry_name("./x.bin").unwrap(), PathBuf::from("x.bin"));
        assert_eq!(sanitize_entry_name("a//b").unwrap(), PathBuf::from("a/b"));
        // 尾部斜杠（目录条目）归一化
        assert_eq!(
            sanitize_entry_name("sub/dir/").unwrap(),
            PathBuf::from("sub/dir")
        );
    }

    #[test]
    fn archive_kind_classification() {
        assert_eq!(classify_archive("m.zip"), Some(ArchiveKind::Zip));
        assert_eq!(classify_archive("M.ZIP"), Some(ArchiveKind::Zip));
        assert_eq!(classify_archive("m.tar.gz"), Some(ArchiveKind::TarGz));
        assert_eq!(classify_archive("m.tgz"), Some(ArchiveKind::TarGz));
        assert_eq!(classify_archive("m.TGZ"), Some(ArchiveKind::TarGz));
        assert_eq!(classify_archive("m.rar"), None);
        assert_eq!(classify_archive(""), None);
    }

    // ── 字节级 zip 构造器（重复条目 / 特殊类型位两类形状 ZipWriter 无法产出）─

    mod raw_zip_support {
        pub(super) struct RawEntry {
            pub(super) name: &'static str,
            pub(super) data: Vec<u8>,
            pub(super) unix_mode: u32,
        }

        fn crc32(data: &[u8]) -> u32 {
            let table = crc32_table();
            let mut crc: u32 = 0xFFFF_FFFF;
            for &b in data {
                crc = (crc >> 8) ^ table[((crc ^ b as u32) & 0xFF) as usize];
            }
            !crc
        }

        fn crc32_table() -> [u32; 256] {
            let mut table = [0u32; 256];
            for (i, slot) in table.iter_mut().enumerate() {
                let mut c = i as u32;
                for _ in 0..8 {
                    c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
                }
                *slot = c;
            }
            table
        }

        /// 构造最小 stored 压缩 zip（测试数据极小，无需 deflate）
        pub(super) fn build(entries: &[RawEntry]) -> Vec<u8> {
            let mut out = Vec::new();
            let mut central = Vec::new();
            for e in entries {
                let offset = out.len() as u32;
                let crc = crc32(&e.data);
                let len = e.data.len() as u32;
                // local file header
                out.extend_from_slice(&[0x50, 0x4B, 0x03, 0x04]);
                out.extend_from_slice(&20u16.to_le_bytes()); // version needed
                out.extend_from_slice(&0u16.to_le_bytes()); // flags
                out.extend_from_slice(&0u16.to_le_bytes()); // method: stored
                out.extend_from_slice(&0u16.to_le_bytes()); // mod time
                out.extend_from_slice(&0x21u16.to_le_bytes()); // mod date: 1980-01-01
                out.extend_from_slice(&crc.to_le_bytes());
                out.extend_from_slice(&len.to_le_bytes()); // compressed size
                out.extend_from_slice(&len.to_le_bytes()); // uncompressed size
                out.extend_from_slice(&(e.name.len() as u16).to_le_bytes());
                out.extend_from_slice(&0u16.to_le_bytes()); // extra len
                out.extend_from_slice(e.name.as_bytes());
                out.extend_from_slice(&e.data);
                // central directory header
                central.extend_from_slice(&[0x50, 0x4B, 0x01, 0x02]);
                central.extend_from_slice(&((3u16 << 8) | 20).to_le_bytes()); // made by: Unix
                central.extend_from_slice(&20u16.to_le_bytes());
                central.extend_from_slice(&0u16.to_le_bytes()); // flags
                central.extend_from_slice(&0u16.to_le_bytes()); // method
                central.extend_from_slice(&0u16.to_le_bytes()); // time
                central.extend_from_slice(&0x21u16.to_le_bytes()); // date
                central.extend_from_slice(&crc.to_le_bytes());
                central.extend_from_slice(&len.to_le_bytes());
                central.extend_from_slice(&len.to_le_bytes());
                central.extend_from_slice(&(e.name.len() as u16).to_le_bytes());
                central.extend_from_slice(&0u16.to_le_bytes()); // extra len
                central.extend_from_slice(&0u16.to_le_bytes()); // comment len
                central.extend_from_slice(&0u16.to_le_bytes()); // disk number
                central.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
                central.extend_from_slice(&(e.unix_mode << 16).to_le_bytes()); // external attrs
                central.extend_from_slice(&offset.to_le_bytes());
                central.extend_from_slice(e.name.as_bytes());
            }
            let cd_offset = out.len() as u32;
            out.extend_from_slice(&central);
            let cd_size = central.len() as u32;
            // EOCD
            out.extend_from_slice(&[0x50, 0x4B, 0x05, 0x06]);
            out.extend_from_slice(&0u16.to_le_bytes()); // disk
            out.extend_from_slice(&0u16.to_le_bytes()); // cd disk
            out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
            out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
            out.extend_from_slice(&cd_size.to_le_bytes());
            out.extend_from_slice(&cd_offset.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes()); // comment len
            out
        }
    }

    use raw_zip_support::RawEntry;

    fn raw_zip(entries: &[RawEntry]) -> Vec<u8> {
        raw_zip_support::build(entries)
    }

    fn dir_size(path: &Path) -> u64 {
        let mut total = 0;
        if let Ok(rd) = std::fs::read_dir(path) {
            for entry in rd.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    total += dir_size(&p);
                } else if let Ok(m) = p.symlink_metadata() {
                    total += m.len();
                }
            }
        }
        total
    }
}
